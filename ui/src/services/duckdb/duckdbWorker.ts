/**
 * DuckDB Web Worker
 *
 * This worker loads and manages a DuckDB WASM instance for executing SQL queries
 * in-browser. Supports CSV, Parquet, JSON, and DuckDB database files via the virtual filesystem.
 *
 * Communication protocol:
 * - Main thread sends { type, id, ... } messages
 * - Worker responds with { type, id, ... } messages
 * - Errors are sent as { type: "error", id, error: string }
 */

import * as duckdb from "@duckdb/duckdb-wasm";

import { formatApiError } from "@/utils/formatApiError";

// DuckDB WASM bundles are self-hosted same-origin (vendored by
// scripts/vendor-wasm.mjs into ui/public/wasm/duckdb/) so the CSP needs no
// third-party script-src. Only mvp + eh are shipped: Hadrian sets no COOP/COEP
// headers, so the browser is never cross-origin-isolated and selectBundle never
// picks the `coi` bundle. Overridable at build time to point back at a CDN.
const DUCKDB_BASE_URL = import.meta.env.VITE_DUCKDB_BASE_URL ?? "/wasm/duckdb/";

const MANUAL_BUNDLES: duckdb.DuckDBBundles = {
  mvp: {
    mainModule: `${DUCKDB_BASE_URL}duckdb-mvp.wasm`,
    mainWorker: `${DUCKDB_BASE_URL}duckdb-browser-mvp.worker.js`,
  },
  eh: {
    mainModule: `${DUCKDB_BASE_URL}duckdb-eh.wasm`,
    mainWorker: `${DUCKDB_BASE_URL}duckdb-browser-eh.worker.js`,
  },
};

/** Message types from main thread to worker */
interface ExecuteMessage {
  type: "execute";
  id: string;
  sql: string;
}

interface RegisterFileMessage {
  type: "registerFile";
  id: string;
  name: string;
  data: ArrayBuffer;
  fileType: "csv" | "parquet" | "json" | "duckdb";
}

interface RegisterDatabaseHandleMessage {
  type: "registerDatabaseHandle";
  id: string;
  name: string;
  handle: File;
}

interface UnregisterFileMessage {
  type: "unregisterFile";
  id: string;
  name: string;
}

interface ListTablesMessage {
  type: "listTables";
  id: string;
}

interface DescribeTableMessage {
  type: "describeTable";
  id: string;
  tableName: string;
}

interface StatusMessage {
  type: "status";
  id: string;
}

type WorkerMessage =
  | ExecuteMessage
  | RegisterFileMessage
  | RegisterDatabaseHandleMessage
  | UnregisterFileMessage
  | ListTablesMessage
  | DescribeTableMessage
  | StatusMessage;

/** Message types from worker to main thread */
interface ReadyResponse {
  type: "ready";
}

interface LoadingResponse {
  type: "loading";
  message: string;
}

interface ExecuteResponse {
  type: "executeResult";
  id: string;
  success: boolean;
  columns: Array<{ name: string; type: string }>;
  rows: Array<Record<string, unknown>>;
  rowCount: number;
  error?: string;
}

interface RegisterFileResponse {
  type: "registerFileResult";
  id: string;
  success: boolean;
  error?: string;
  dbAlias?: string;
}

interface UnregisterFileResponse {
  type: "unregisterFileResult";
  id: string;
  success: boolean;
  error?: string;
}

interface ListTablesResponse {
  type: "listTablesResult";
  id: string;
  success: boolean;
  tables: Array<{ schema: string; name: string; type: string }>;
  error?: string;
}

interface DescribeTableResponse {
  type: "describeTableResult";
  id: string;
  success: boolean;
  columns: Array<{ name: string; type: string; nullable: boolean }>;
  error?: string;
}

interface StatusResponse {
  type: "statusResult";
  id: string;
  ready: boolean;
  registeredFiles: string[];
}

interface ErrorResponse {
  type: "error";
  id?: string;
  error: string;
}

type WorkerResponse =
  | ReadyResponse
  | LoadingResponse
  | ExecuteResponse
  | RegisterFileResponse
  | UnregisterFileResponse
  | ListTablesResponse
  | DescribeTableResponse
  | StatusResponse
  | ErrorResponse;

// Worker state
let db: duckdb.AsyncDuckDB | null = null;
let conn: duckdb.AsyncDuckDBConnection | null = null;
let isLoading = false;
const registeredFiles = new Set<string>();
/** Tracks attached DuckDB databases: filename -> alias */
const attachedDatabases = new Map<string, string>();

/**
 * Send a message to the main thread
 */
function sendMessage(message: WorkerResponse) {
  self.postMessage(message);
}

/**
 * Initialize DuckDB WASM
 */
async function initDuckDB(): Promise<duckdb.AsyncDuckDB> {
  if (db) return db;
  if (isLoading) {
    // Wait for existing load
    while (isLoading) {
      await new Promise((resolve) => setTimeout(resolve, 100));
    }
    if (db) return db;
    throw new Error("DuckDB initialization failed");
  }

  isLoading = true;
  sendMessage({ type: "loading", message: "Loading DuckDB WASM..." });

  try {
    // Select the best self-hosted bundle for the current browser
    // (eh on modern browsers, mvp as fallback).
    const bundle = await duckdb.selectBundle(MANUAL_BUNDLES);

    sendMessage({ type: "loading", message: "Initializing database..." });

    // Create worker URL for DuckDB's internal worker
    const workerUrl = URL.createObjectURL(
      new Blob([`importScripts("${bundle.mainWorker!}");`], { type: "text/javascript" })
    );

    // Initialize DuckDB
    const worker = new Worker(workerUrl);
    const logger = new duckdb.ConsoleLogger(duckdb.LogLevel.WARNING);
    db = new duckdb.AsyncDuckDB(logger, worker);
    await db.instantiate(bundle.mainModule, bundle.pthreadWorker);

    // Clean up the blob URL
    URL.revokeObjectURL(workerUrl);

    // Create a connection
    conn = await db.connect();

    sendMessage({ type: "ready" });
    return db;
  } catch (error) {
    const errorMsg = error instanceof Error ? error.message : formatApiError(error);
    sendMessage({ type: "error", error: `Failed to load DuckDB: ${errorMsg}` });
    throw error;
  } finally {
    isLoading = false;
  }
}

/**
 * Execute a SQL query and return results
 */
async function executeQuery(sql: string): Promise<{
  success: boolean;
  columns: Array<{ name: string; type: string }>;
  rows: Array<Record<string, unknown>>;
  rowCount: number;
  error?: string;
}> {
  await initDuckDB();

  if (!conn) {
    throw new Error("No database connection");
  }

  try {
    const result = await conn.query(sql);

    // Extract column info from Arrow schema
    const columns = result.schema.fields.map((field) => ({
      name: field.name,
      type: String(field.type),
    }));

    // Convert Arrow table to plain JS objects
    const rows: Array<Record<string, unknown>> = [];
    for (const row of result) {
      const rowObj: Record<string, unknown> = {};
      for (const field of result.schema.fields) {
        const value = row[field.name];
        // Convert BigInt to number for JSON compatibility
        if (typeof value === "bigint") {
          rowObj[field.name] = Number(value);
        } else {
          rowObj[field.name] = value;
        }
      }
      rows.push(rowObj);
    }

    return {
      success: true,
      columns,
      rows,
      rowCount: rows.length,
    };
  } catch (error) {
    const errorMsg = error instanceof Error ? error.message : formatApiError(error);
    return {
      success: false,
      columns: [],
      rows: [],
      rowCount: 0,
      error: errorMsg,
    };
  }
}

/**
 * Derive a safe, unique database alias from a filename (e.g., "my-data.duckdb" -> "my_data").
 * Appends a counter suffix when the alias already exists in attachedDatabases.
 */
function deriveDbAlias(filename: string): string {
  const base =
    filename
      .replace(/\.duckdb$/i, "")
      .replace(/[^a-zA-Z0-9_]/g, "_")
      .replace(/_+/g, "_")
      .replace(/^_|_$/g, "") || "db";

  const existing = new Set(attachedDatabases.values());
  if (!existing.has(base)) return base;
  let i = 2;
  while (existing.has(`${base}_${i}`)) i++;
  return `${base}_${i}`;
}

/**
 * Register a file in DuckDB's virtual filesystem
 */
async function registerFile(
  name: string,
  data: ArrayBuffer,
  fileType: "csv" | "parquet" | "json" | "duckdb"
): Promise<{ success: boolean; error?: string; dbAlias?: string }> {
  await initDuckDB();

  if (!db || !conn) {
    return { success: false, error: "Database not initialized" };
  }

  try {
    const dataSize = data.byteLength;

    if (dataSize === 0) {
      return { success: false, error: "File data is empty" };
    }

    // Register the file buffer
    await db.registerFileBuffer(name, new Uint8Array(data));

    // For .duckdb files, attach the database so its tables are queryable
    if (fileType === "duckdb") {
      const alias = deriveDbAlias(name);
      const escapedName = name.replace(/'/g, "''");
      try {
        await conn.query(`ATTACH '${escapedName}' AS "${alias}" (READ_ONLY)`);
      } catch (attachError) {
        await db.dropFile(name);
        throw attachError;
      }
      registeredFiles.add(name);
      attachedDatabases.set(name, alias);
      return { success: true, dbAlias: alias };
    }

    registeredFiles.add(name);
    return { success: true };
  } catch (error) {
    const errorMsg = error instanceof Error ? error.message : formatApiError(error);
    return { success: false, error: errorMsg };
  }
}

/**
 * Register a database file via BROWSER_FILEREADER protocol.
 * DuckDB reads lazily from the File handle on demand — no memory overhead.
 */
async function registerDatabaseHandle(
  name: string,
  handle: File
): Promise<{ success: boolean; error?: string; dbAlias?: string }> {
  await initDuckDB();

  if (!db || !conn) {
    return { success: false, error: "Database not initialized" };
  }

  try {
    await db.registerFileHandle(name, handle, duckdb.DuckDBDataProtocol.BROWSER_FILEREADER, true);

    const alias = deriveDbAlias(name);
    const escapedName = name.replace(/'/g, "''");
    try {
      await conn.query(`ATTACH '${escapedName}' AS "${alias}" (READ_ONLY)`);
    } catch (attachError) {
      await db.dropFile(name);
      throw attachError;
    }
    registeredFiles.add(name);
    attachedDatabases.set(name, alias);
    return { success: true, dbAlias: alias };
  } catch (error) {
    const errorMsg = error instanceof Error ? error.message : formatApiError(error);
    return { success: false, error: errorMsg };
  }
}

/**
 * Unregister a file from DuckDB's virtual filesystem
 */
async function unregisterFile(name: string): Promise<{ success: boolean; error?: string }> {
  if (!db) {
    return { success: false, error: "Database not initialized" };
  }

  try {
    // Detach if it was an attached database
    const alias = attachedDatabases.get(name);
    if (alias && conn) {
      await conn.query(`DETACH "${alias}"`);
      attachedDatabases.delete(name);
    }

    await db.dropFile(name);
    registeredFiles.delete(name);
    return { success: true };
  } catch (error) {
    const errorMsg = error instanceof Error ? error.message : formatApiError(error);
    return { success: false, error: errorMsg };
  }
}

/**
 * List all tables in the database
 */
async function listTables(): Promise<{
  success: boolean;
  tables: Array<{ schema: string; name: string; type: string }>;
  error?: string;
}> {
  await initDuckDB();

  if (!conn) {
    return { success: false, tables: [], error: "No database connection" };
  }

  try {
    const result = await conn.query(`
      SELECT table_schema, table_name, table_type
      FROM information_schema.tables
      WHERE table_schema NOT IN ('information_schema', 'pg_catalog')
      ORDER BY table_schema, table_name
    `);

    const tables: Array<{ schema: string; name: string; type: string }> = [];
    for (const row of result) {
      tables.push({
        schema: String(row.table_schema),
        name: String(row.table_name),
        type: String(row.table_type),
      });
    }

    return { success: true, tables };
  } catch (error) {
    const errorMsg = error instanceof Error ? error.message : formatApiError(error);
    return { success: false, tables: [], error: errorMsg };
  }
}

/**
 * Describe a table's schema (columns and types)
 *
 * Uses DESCRIBE SELECT for files (e.g., 'data.csv') or information_schema for tables.
 */
async function describeTable(tableName: string): Promise<{
  success: boolean;
  columns: Array<{ name: string; type: string; nullable: boolean }>;
  error?: string;
}> {
  await initDuckDB();

  if (!conn) {
    return { success: false, columns: [], error: "No database connection" };
  }

  try {
    // For files (quoted names like 'data.csv'), use DESCRIBE SELECT
    // For tables, we can also use DESCRIBE which works for both
    const result = await conn.query(`DESCRIBE SELECT * FROM ${tableName}`);

    const columns: Array<{ name: string; type: string; nullable: boolean }> = [];
    for (const row of result) {
      columns.push({
        name: String(row.column_name),
        type: String(row.column_type),
        // DESCRIBE doesn't provide nullable info directly, default to true
        nullable: true,
      });
    }

    return { success: true, columns };
  } catch (error) {
    const errorMsg = error instanceof Error ? error.message : formatApiError(error);
    return { success: false, columns: [], error: errorMsg };
  }
}

/**
 * Handle messages from the main thread
 */
self.onmessage = async (event: MessageEvent<WorkerMessage>) => {
  const message = event.data;

  switch (message.type) {
    case "execute": {
      try {
        const result = await executeQuery(message.sql);
        sendMessage({
          type: "executeResult",
          id: message.id,
          ...result,
        });
      } catch (error) {
        const errorMsg = error instanceof Error ? error.message : formatApiError(error);
        sendMessage({
          type: "error",
          id: message.id,
          error: errorMsg,
        });
      }
      break;
    }

    case "registerFile": {
      try {
        const result = await registerFile(message.name, message.data, message.fileType);
        sendMessage({
          type: "registerFileResult",
          id: message.id,
          ...result,
        });
      } catch (error) {
        const errorMsg = error instanceof Error ? error.message : formatApiError(error);
        sendMessage({
          type: "error",
          id: message.id,
          error: errorMsg,
        });
      }
      break;
    }

    case "registerDatabaseHandle": {
      try {
        const result = await registerDatabaseHandle(message.name, message.handle);
        sendMessage({
          type: "registerFileResult",
          id: message.id,
          ...result,
        });
      } catch (error) {
        const errorMsg = error instanceof Error ? error.message : formatApiError(error);
        sendMessage({
          type: "error",
          id: message.id,
          error: errorMsg,
        });
      }
      break;
    }

    case "unregisterFile": {
      try {
        const result = await unregisterFile(message.name);
        sendMessage({
          type: "unregisterFileResult",
          id: message.id,
          ...result,
        });
      } catch (error) {
        const errorMsg = error instanceof Error ? error.message : formatApiError(error);
        sendMessage({
          type: "error",
          id: message.id,
          error: errorMsg,
        });
      }
      break;
    }

    case "listTables": {
      try {
        const result = await listTables();
        sendMessage({
          type: "listTablesResult",
          id: message.id,
          ...result,
        });
      } catch (error) {
        const errorMsg = error instanceof Error ? error.message : formatApiError(error);
        sendMessage({
          type: "error",
          id: message.id,
          error: errorMsg,
        });
      }
      break;
    }

    case "describeTable": {
      try {
        const result = await describeTable(message.tableName);
        sendMessage({
          type: "describeTableResult",
          id: message.id,
          ...result,
        });
      } catch (error) {
        const errorMsg = error instanceof Error ? error.message : formatApiError(error);
        sendMessage({
          type: "error",
          id: message.id,
          error: errorMsg,
        });
      }
      break;
    }

    case "status": {
      sendMessage({
        type: "statusResult",
        id: message.id,
        ready: db !== null && conn !== null,
        registeredFiles: Array.from(registeredFiles),
      });
      break;
    }

    default:
      sendMessage({
        type: "error",
        error: `Unknown message type: ${(message as { type: string }).type}`,
      });
  }
};

// Start loading DuckDB immediately
initDuckDB().catch(console.error);
