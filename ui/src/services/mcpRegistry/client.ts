/**
 * Client for the official MCP Registry v0.1 API.
 *
 * The registry is CORS-enabled (access-control-allow-origin: *), so the
 * browser can call it directly — no backend proxy required.
 */

import type {
  MCPRegistryEntry,
  MCPRegistryPackage,
  MCPRegistryPackageArgument,
  MCPRegistryRemote,
  MCPRegistrySearchResponse,
} from "./types";

const REGISTRY_BASE = "https://registry.modelcontextprotocol.io/v0.1";

export interface SearchRegistryOptions {
  search?: string;
  limit?: number;
  cursor?: string;
  signal?: AbortSignal;
}

export async function searchRegistry(
  options: SearchRegistryOptions = {}
): Promise<MCPRegistrySearchResponse> {
  const params = new URLSearchParams();
  if (options.search) params.set("search", options.search);
  if (options.limit != null) params.set("limit", String(options.limit));
  if (options.cursor) params.set("cursor", options.cursor);

  const url = `${REGISTRY_BASE}/servers${params.toString() ? `?${params}` : ""}`;
  const res = await fetch(url, { signal: options.signal });
  if (!res.ok) {
    throw new Error(`Registry request failed: ${res.status} ${res.statusText}`);
  }
  return (await res.json()) as MCPRegistrySearchResponse;
}

/**
 * Fetch the latest version of a specific registry entry by name (e.g.
 * `io.github.hadriangateway/platter`). Used to resolve favorite references into
 * full entries so the catalog can render them alongside search results.
 */
export async function getRegistryEntry(
  name: string,
  signal?: AbortSignal
): Promise<MCPRegistryEntry> {
  const url = `${REGISTRY_BASE}/servers/${encodeURIComponent(name)}/versions/latest`;
  const res = await fetch(url, { signal });
  if (!res.ok) {
    throw new Error(`Registry request failed: ${res.status} ${res.statusText}`);
  }
  return (await res.json()) as MCPRegistryEntry;
}

/** An entry categorized by how it can be connected. */
export type CategorizedEntry =
  | { kind: "remote"; entry: MCPRegistryEntry; remotes: MCPRegistryRemote[] }
  | { kind: "local"; entry: MCPRegistryEntry; packages: MCPRegistryPackage[] };

/**
 * Classify an entry. Remote wins: if a server ships both remotes and packages,
 * we prefer the remote path because it's directly connectable from the browser.
 */
export function categorize(entry: MCPRegistryEntry): CategorizedEntry | null {
  const remotes = entry.server.remotes ?? [];
  if (remotes.length > 0) {
    return { kind: "remote", entry, remotes };
  }
  const packages = entry.server.packages ?? [];
  if (packages.length > 0) {
    return { kind: "local", entry, packages };
  }
  return null;
}

/**
 * Dedupe registry results by server.name, keeping only the latest version
 * when a server appears multiple times. Registry responses include every
 * published version, so filtering is essential.
 */
export function dedupeLatest(entries: MCPRegistryEntry[]): MCPRegistryEntry[] {
  const byName = new Map<string, MCPRegistryEntry>();
  for (const e of entries) {
    const existing = byName.get(e.server.name);
    const isLatest = e._meta?.["io.modelcontextprotocol.registry/official"]?.isLatest;
    if (!existing) {
      byName.set(e.server.name, e);
    } else if (isLatest) {
      byName.set(e.server.name, e);
    }
  }
  return Array.from(byName.values());
}

/**
 * Package registry types we know how to launch, in preference order. Entries
 * often ship multiple packages (e.g. npm + oci); we pick the easiest-to-run
 * one that the user probably has tooling for.
 */
const PACKAGE_PREFERENCE: readonly string[] = ["npm", "pypi", "oci"];

export function pickPreferredPackage(packages: MCPRegistryPackage[]): MCPRegistryPackage | null {
  // A single server may publish several packages per registryType (e.g. an
  // stdio OCI image and a streamable-http OCI image). Prefer the richer
  // variant — one with runtime or package arguments set — since it's more
  // likely to be the maintainer's intended launch configuration.
  const richness = (p: MCPRegistryPackage): number =>
    (p.runtimeArguments?.length ?? 0) + (p.packageArguments?.length ?? 0);

  for (const type of PACKAGE_PREFERENCE) {
    const matches = packages.filter((p) => p.registryType === type);
    if (matches.length === 0) continue;
    matches.sort((a, b) => richness(b) - richness(a));
    return matches[0];
  }
  return null;
}

function renderArg(a: MCPRegistryPackageArgument): string | null {
  if (a.type === "named" && a.name) {
    return a.value ? `${a.name} ${a.value}` : a.name;
  }
  return a.value ?? a.name ?? null;
}

function joinArgs(args: MCPRegistryPackageArgument[] | undefined): string {
  const rendered = (args ?? []).map(renderArg).filter((v): v is string => Boolean(v));
  return rendered.length > 0 ? ` ${rendered.join(" ")}` : "";
}

/** Produce the raw launch command (no supergateway wrapper). */
function buildStdioCommand(pkg: MCPRegistryPackage): string | null {
  const runtimeArgs = joinArgs(pkg.runtimeArguments);
  const packageArgs = joinArgs(pkg.packageArguments);

  switch (pkg.registryType) {
    case "npm":
      return `npx -y${runtimeArgs} ${pkg.identifier}${pkg.version ? `@${pkg.version}` : ""}${packageArgs}`;
    case "pypi":
      return `uvx${runtimeArgs} ${pkg.identifier}${pkg.version ? `==${pkg.version}` : ""}${packageArgs}`;
    case "oci":
      return `docker run --rm -i${runtimeArgs} ${pkg.identifier}${pkg.version ? `:${pkg.version}` : ""}${packageArgs}`;
    default:
      return null;
  }
}

/**
 * Default URL to connect to a supergateway-wrapped local server.
 *
 * Note: supergateway binds a fixed port, so only one stdio-wrapped server can
 * run at a time with this default. Users who need multiple concurrent local
 * servers must edit the generated command to pass a different `--port`.
 */
const SUPERGATEWAY_DEFAULT_URL = "http://localhost:8000/mcp";

export interface InstallCommand {
  /** The full shell command to run locally. */
  command: string;
  /** The URL to configure as the MCP server endpoint once the command is running. */
  url: string;
}

/**
 * Build the shell command and target URL for a local package.
 *
 * When the package already speaks streamable-http (e.g. a docker image that
 * serves HTTP directly), the raw launch command is returned along with the
 * URL the package advertises. Otherwise the command is wrapped with
 * supergateway so a stdio process is exposed as streamable-http, and the URL
 * defaults to supergateway's `localhost:8000/mcp`.
 *
 * Returns null if the registryType isn't supported.
 */
export function buildInstallCommand(pkg: MCPRegistryPackage): InstallCommand | null {
  const raw = buildStdioCommand(pkg);
  if (!raw) return null;

  if (pkg.transport?.type === "streamable-http" && pkg.transport.url) {
    return { command: raw, url: pkg.transport.url };
  }
  // Single-quote wrap `raw` so registry-supplied strings can't break out of
  // the --stdio argument. Any embedded `'` is escaped via the POSIX trick of
  // closing the quote, inserting an escaped quote, and reopening: `'\''`.
  const quoted = `'${raw.replace(/'/g, "'\\''")}'`;
  return {
    command: `npx -y supergateway --cors "*" --outputTransport streamableHttp --stdio ${quoted}`,
    url: SUPERGATEWAY_DEFAULT_URL,
  };
}

/**
 * Resolve templated header values (e.g. "Bearer {smithery_api_key}") into a
 * flat header map, leaving placeholders verbatim so the user sees what to
 * replace. Required headers without a preset value get a `{header_name}`
 * placeholder so the form surfaces them and the template-warning banner
 * triggers — otherwise the user never sees that a required header is missing.
 */
export function materializeHeaders(remote: MCPRegistryRemote): Record<string, string> {
  const out: Record<string, string> = {};
  for (const h of remote.headers ?? []) {
    if (h.value != null) {
      out[h.name] = h.value;
    } else if (h.isRequired) {
      out[h.name] = `{${h.name}}`;
    }
  }
  return out;
}

/** Does any value in the header map contain an unfilled `{placeholder}`? */
export function hasUnfilledTemplate(headers: Record<string, string>): boolean {
  return Object.values(headers).some((v) => /\{[^}]+\}/.test(v));
}
