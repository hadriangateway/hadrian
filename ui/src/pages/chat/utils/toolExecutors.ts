/**
 * Tool Executors - Extensible system for client-side tool execution
 *
 * This module provides an extensible architecture for executing tools client-side.
 * Each tool type has its own executor that handles the specific API calls and
 * result formatting.
 *
 * ## Adding a New Tool
 *
 * 1. Create an executor function matching the `ToolExecutor` signature
 * 2. Register it in `defaultToolExecutors` or pass custom executors to `executeTools`
 *
 * ## Example: Adding a hypothetical `translation` tool
 *
 * ```typescript
 * const translationExecutor: ToolExecutor = async (toolCall, context) => {
 *   if (toolCall.name !== "translation") {
 *     return { success: false, error: "Invalid tool type" };
 *   }
 *   const args = toolCall.arguments as { text: string; target_lang: string };
 *   const result = await translateText(args.text, args.target_lang);
 *   return {
 *     success: true,
 *     output: JSON.stringify(result),
 *   };
 * };
 *
 * // Register it
 * const executors = {
 *   ...defaultToolExecutors,
 *   translation: translationExecutor,
 * };
 * ```
 */

import type { Citation, ChunkCitation } from "@/components/chat-types";
import type { ParsedToolCall, FileSearchToolCall } from "./toolCallParser";
import { invalidArgumentsText } from "./toolCallParser";
import { executeFileSearch, type ExecuteFileSearchOptions } from "./executeFileSearch";
import { pyodideService } from "@/services/pyodide";
import { quickjsService } from "@/services/quickjs";
import { duckdbService } from "@/services/duckdb";
import { callMCPTool } from "@/stores/mcpStore";
import { skillExecutor } from "./skillExecutor";
import type { ToolContent } from "@/services/mcp";
import safeRegex from "safe-regex";

import { formatApiError } from "@/utils/formatApiError";
import { getAgenticGuidance } from "@/utils/defaultSystemPrompt";
/**
 * Context provided to tool executors
 */
export interface ToolExecutorContext {
  /** Vector store IDs attached to the conversation (for file_search) */
  vectorStoreIds?: string[];
  /** Auth token for API calls */
  token?: string;
  /** Abort signal for cancellation */
  signal?: AbortSignal;
  /**
   * Callback to report status messages during execution.
   * Used to show progress like "Loading Python runtime..." or "Downloading packages...".
   * @param toolCallId - The ID of the tool call being executed
   * @param message - The status message to display
   */
  onStatusMessage?: (toolCallId: string, message: string) => void;
  /** Default model for sub-agent tool when no model is specified in arguments */
  defaultModel?: string;
}

/**
 * Artifact types that can be produced by tool execution
 * - "display_selection" is a special meta-artifact that indicates which artifacts to show prominently
 * - "agent" displays sub-agent task and response in a collapsible card
 * - "file_search" displays search query and results from knowledge base search
 */
export type ArtifactType =
  | "code"
  | "table"
  | "chart"
  | "image"
  | "html"
  | "agent"
  | "file_search"
  | "container_file"
  | "display_selection";

/**
 * Container file artifact data — a file written to `/mnt/data` by the shell
 * tool. The bytes aren't inlined; the renderer lazily fetches them from
 * `GET /v1/containers/{containerId}/files/{fileId}/content` (authed), showing
 * images inline and other files as a download chip.
 */
export interface ContainerFileArtifactData {
  containerId: string;
  fileId: string;
  filename: string;
  /** Best-effort MIME type (e.g. "image/png"). */
  contentType?: string;
  /** Size in bytes, for the download chip. */
  bytes?: number;
}

/**
 * Role of an artifact in the execution timeline
 * - 'input': Code, queries, or other inputs shown to the tool (collapsed by default)
 * - 'output': Charts, tables, images, or results shown prominently
 */
export type ArtifactRole = "input" | "output";

/**
 * An artifact produced by tool execution
 *
 * Artifacts are rich output objects that are displayed in the UI but not sent
 * back to the model. They allow tools to produce visual output like charts,
 * tables, code blocks, and images.
 *
 * ## Examples
 *
 * Code output:
 * ```typescript
 * { id: "code-1", type: "code", title: "Output", data: { language: "python", code: "..." } }
 * ```
 *
 * Table output:
 * ```typescript
 * { id: "table-1", type: "table", title: "Results", data: { columns: [...], rows: [...] } }
 * ```
 *
 * Image output:
 * ```typescript
 * { id: "img-1", type: "image", data: "data:image/png;base64,...", mimeType: "image/png" }
 * ```
 */
export interface Artifact {
  /** Unique identifier for this artifact */
  id: string;
  /** The type of artifact (determines how it's rendered) */
  type: ArtifactType;
  /** Optional title displayed above the artifact */
  title?: string;
  /** Type-specific data for rendering the artifact */
  data: unknown;
  /** MIME type for binary data (images, files) */
  mimeType?: string;
  /** Role in execution timeline: 'input' (code/query) or 'output' (results/charts) */
  role?: ArtifactRole;
  /** ID of the tool call that produced this artifact */
  toolCallId?: string;
  /** Order within the execution (for timeline display) */
  stepIndex?: number;
}

/**
 * Code artifact data
 */
export interface CodeArtifactData {
  /** Programming language for syntax highlighting */
  language: string;
  /** The code content */
  code: string;
}

/**
 * Table artifact data
 */
export interface TableArtifactData {
  /** Column definitions */
  columns: Array<{ key: string; label: string }>;
  /** Row data (array of objects matching column keys) */
  rows: Array<Record<string, unknown>>;
}

/**
 * Chart artifact data (Vega-Lite spec)
 */
export interface ChartArtifactData {
  /** Vega-Lite specification */
  spec: Record<string, unknown>;
}

/**
 * Display selection artifact data - indicates which artifacts to show prominently
 */
export interface DisplaySelectionData {
  /** Artifact IDs to display prominently (in order) */
  artifactIds: string[];
  /** Layout mode for displayed artifacts */
  layout: "inline" | "gallery" | "stacked";
}

/**
 * Inline display directive — emitted by the model as a top-level `display` field
 * on any artifact-producing tool call. Saves a round-trip versus calling
 * `display_artifacts` separately after each tool completes.
 */
export interface DisplayDirective {
  when: "always" | "on_success" | "on_error" | "if_output_matches" | "never";
  /** Regex tested against `result.output` when `when === "if_output_matches"`. */
  match?: string;
  /** Layout for the auto-selected artifacts. Defaults to "inline". */
  layout?: "inline" | "gallery" | "stacked";
}

// Bounds for the LLM-supplied `match` regex used by `if_output_matches`.
// Keep these in sync with the schema description below so the model knows
// the limits up front.
const MAX_DISPLAY_MATCH_PATTERN_LEN = 256;
const MAX_DISPLAY_MATCH_INPUT_LEN = 16_384;

/**
 * JSON Schema fragment for the `display` parameter. Inject this as a property
 * on any tool that can produce artifacts (both built-in and MCP).
 */
export const DISPLAY_PARAMETER_SCHEMA = {
  type: "object",
  description:
    "Optional. Auto-display this tool's output artifacts inline when the condition is met, " +
    "skipping the need for a follow-up display_artifacts call. Omit to keep artifacts in " +
    "the collapsed 'more outputs' section unless you call display_artifacts explicitly.",
  properties: {
    when: {
      type: "string",
      enum: ["always", "on_success", "on_error", "if_output_matches", "never"],
      description:
        "'always': display regardless of outcome. 'on_success': only when the tool succeeds. " +
        "'on_error': only on failure. 'if_output_matches': test `match` regex against output. " +
        "'never': suppress inline display.",
    },
    match: {
      type: "string",
      maxLength: MAX_DISPLAY_MATCH_PATTERN_LEN,
      description:
        `Regex tested against the tool output (required when when='if_output_matches'). ` +
        `Maximum ${MAX_DISPLAY_MATCH_PATTERN_LEN} characters. Patterns prone to catastrophic ` +
        `backtracking (e.g. nested quantifiers like \`(a+)+\`) are rejected. The output is ` +
        `truncated to ${MAX_DISPLAY_MATCH_INPUT_LEN} characters before matching, so anchor ` +
        `near the start or use a non-anchored pattern.`,
    },
    layout: {
      type: "string",
      enum: ["inline", "gallery", "stacked"],
      description: "Layout for auto-displayed artifacts. Defaults to 'inline'.",
    },
  },
  required: ["when"],
} as const;

function shouldApplyDisplay(directive: DisplayDirective, result: ToolExecutionResult): boolean {
  switch (directive.when) {
    case "always":
      return true;
    case "never":
      return false;
    case "on_success":
      return result.success === true;
    case "on_error":
      return result.success === false;
    case "if_output_matches": {
      if (!directive.match) return false;
      // The pattern comes from LLM tool-call arguments, so it's untrusted.
      // Cap pattern + input length and screen out catastrophic-backtracking
      // patterns with safe-regex before running them.
      if (directive.match.length > MAX_DISPLAY_MATCH_PATTERN_LEN) {
        console.warn(
          `display.match pattern exceeds ${MAX_DISPLAY_MATCH_PATTERN_LEN} chars; skipping`
        );
        return false;
      }
      if (!safeRegex(directive.match)) {
        console.warn(`display.match pattern rejected as ReDoS-prone: ${directive.match}`);
        return false;
      }
      try {
        const input = (result.output ?? "").slice(0, MAX_DISPLAY_MATCH_INPUT_LEN);
        return new RegExp(directive.match).test(input);
      } catch (err) {
        console.warn(`Invalid regex in display.match (${directive.match}):`, err);
        return false;
      }
    }
    default:
      return false;
  }
}

/**
 * If the tool call carries a `display` directive and it evaluates to true,
 * synthesise a display_selection artifact pointing at every non-meta artifact
 * the tool produced. Returns the result unchanged otherwise.
 */
export function applyInlineDisplay(
  toolCall: ParsedToolCall,
  result: ToolExecutionResult
): ToolExecutionResult {
  const args = toolCall.arguments as Record<string, unknown> | undefined;
  const raw = args?.display;
  if (!raw || typeof raw !== "object") return result;
  const directive = raw as DisplayDirective;
  if (!shouldApplyDisplay(directive, result)) return result;

  const displayableIds = (result.artifacts ?? [])
    .filter((a) => a.type !== "display_selection")
    .map((a) => a.id);
  if (displayableIds.length === 0) return result;

  const toolId = toolCall.id || `display-${Date.now()}-${Math.random().toString(36).slice(2, 9)}`;
  const selectionArtifact: Artifact = {
    id: `display-selection-${toolId}`,
    type: "display_selection",
    title: "Display Selection",
    role: "output",
    toolCallId: toolId,
    data: {
      artifactIds: displayableIds,
      layout: directive.layout ?? "inline",
    } satisfies DisplaySelectionData,
  };

  return {
    ...result,
    artifacts: [...(result.artifacts ?? []), selectionArtifact],
  };
}

/**
 * Remove the `display` field from tool arguments. Used before forwarding
 * arguments to external servers (MCP) so they never see the UI-only field.
 */
export function stripDisplayArg(
  args: Record<string, unknown> | undefined
): Record<string, unknown> | undefined {
  if (!args || !("display" in args)) return args;
  const { display: _discard, ...rest } = args;
  return rest;
}

/**
 * Agent artifact data - displays sub-agent task, internal reasoning, and curated output
 */
export interface AgentArtifactData {
  /** The task that was delegated to the sub-agent */
  task: string;
  /** The model that handled the task */
  model: string;
  /** The sub-agent's internal reasoning/investigation (not sent to main model) */
  internal: string;
  /** The curated output sent back to the main model */
  output: string;
  /** Token usage for the sub-agent calls (both phases combined) */
  usage?: {
    inputTokens: number;
    outputTokens: number;
    totalTokens: number;
    cost?: number;
  };
}

/**
 * File search result item for artifact display
 */
export interface FileSearchResultItem {
  /** File ID the result came from */
  fileId: string;
  /** Filename for display */
  filename: string;
  /** Relevance score (0-1) */
  score: number;
  /** The matched content */
  content: string;
}

/**
 * File search artifact data - displays search query and results
 */
export interface FileSearchArtifactData {
  /** The search query */
  query: string;
  /** Vector store IDs that were searched */
  vectorStoreIds: string[];
  /** Search results */
  results: FileSearchResultItem[];
  /** Total number of results found */
  totalResults: number;
}

/**
 * Status of a tool execution
 */
export type ToolExecutionStatus = "pending" | "running" | "success" | "error";

/**
 * A single tool execution within the timeline
 *
 * Tracks the execution of one tool call, including its input, output,
 * timing, and any artifacts produced.
 */
export interface ToolExecution {
  /** Unique identifier (matches tool call ID) */
  id: string;
  /** Name of the tool (e.g., "code_interpreter", "sql_query") */
  toolName: string;
  /** Current execution status */
  status: ToolExecutionStatus;
  /** When execution started (Unix timestamp ms) */
  startTime: number;
  /** When execution ended (Unix timestamp ms) */
  endTime?: number;
  /** Execution duration in milliseconds */
  duration?: number;
  /** Input parameters passed to the tool */
  input: unknown;
  /** Artifacts representing input (code, queries) - collapsed by default */
  inputArtifacts: Artifact[];
  /** Artifacts representing output (charts, tables, images) - shown prominently */
  outputArtifacts: Artifact[];
  /** Error message if execution failed */
  error?: string;
  /** Which iteration/round this execution belongs to (1-indexed) */
  round: number;
  /** Current status message (e.g., "Loading Python runtime...", "Executing code...") */
  statusMessage?: string;
}

/**
 * A round of tool executions within a multi-turn conversation
 *
 * During multi-turn tool execution, the model may call tools multiple times.
 * Each round represents one iteration of the model calling tools and receiving results.
 * Between rounds, the model may provide reasoning about what to try next.
 */
export interface ToolExecutionRound {
  /** Round number (1-indexed) */
  round: number;
  /** Tool executions in this round (may be multiple if parallel execution) */
  executions: ToolExecution[];
  /** Model's reasoning text between this round and the next (if any) */
  modelReasoning?: string;
  /** Whether any execution in this round failed */
  hasError?: boolean;
  /** Total duration of all executions in this round (ms) */
  totalDuration?: number;
}

/**
 * Result from executing a tool
 */
export interface ToolExecutionResult {
  /** Whether execution succeeded */
  success: boolean;
  /** The output to send back to the model (JSON string) */
  output?: string;
  /** Error message if execution failed */
  error?: string;
  /** Citations extracted from the tool result (for file_search, web_search) */
  citations?: Citation[];
  /** Artifacts produced by the tool (displayed in UI, not sent to model) */
  artifacts?: Artifact[];
}

/**
 * A tool executor function
 *
 * Takes a parsed tool call and context, returns the execution result.
 * The output should be a JSON string that will be sent back to the model.
 */
export type ToolExecutor = (
  toolCall: ParsedToolCall,
  context: ToolExecutorContext
) => Promise<ToolExecutionResult>;

/**
 * Registry of tool executors by tool name
 */
export type ToolExecutorRegistry = Record<string, ToolExecutor>;

/**
 * Convert file search results to Citation objects for UI display
 */
function convertFileSearchResultsToCitations(
  results: Array<{
    file_id: string;
    filename: string;
    score: number;
    content: Array<{ type: string; text: string }>;
  }>
): Citation[] {
  return results.map(
    (result, index): ChunkCitation => ({
      id: `citation-${result.file_id}-${index}`,
      type: "chunk",
      fileId: result.file_id,
      filename: result.filename,
      score: result.score,
      chunkIndex: index,
      content: result.content[0]?.text ?? "",
    })
  );
}

/**
 * Execute the file_search tool
 *
 * Uses the conversation's attached vector stores or the ones specified
 * in the tool call arguments.
 */
export const fileSearchExecutor: ToolExecutor = async (
  toolCall,
  context
): Promise<ToolExecutionResult> => {
  // Type guard for file_search
  if (toolCall.name !== "file_search") {
    return {
      success: false,
      error: `Expected file_search tool, got ${toolCall.name}`,
    };
  }

  const fileSearchCall = toolCall as FileSearchToolCall;
  const args = fileSearchCall.arguments;

  // Determine which vector stores to search
  // Priority: tool call args > conversation context
  const vectorStoreIds = args.vector_store_ids ?? context.vectorStoreIds;

  if (!vectorStoreIds || vectorStoreIds.length === 0) {
    return {
      success: false,
      error: "No vector stores specified for file_search",
      output: JSON.stringify({
        error: "No vector stores available. Please attach a knowledge base to the conversation.",
      }),
    };
  }

  // Build search options
  // Use a low default threshold (0.0) to return all relevant results
  // The LLM typically knows what it's looking for and can filter by relevance
  const searchOptions: ExecuteFileSearchOptions = {
    vectorStoreIds,
    query: args.query,
    maxResults: args.max_num_results ?? 10,
    scoreThreshold: args.score_threshold ?? 0.0,
  };

  // Execute the search
  const result = await executeFileSearch(searchOptions);

  if (result.success) {
    // Convert search results to citations for UI display
    const citations = convertFileSearchResultsToCitations(result.results);

    // Build artifact data for the file search results
    const artifactData: FileSearchArtifactData = {
      query: args.query,
      vectorStoreIds,
      results: result.results.map((r) => ({
        fileId: r.file_id,
        filename: r.filename,
        score: r.score,
        content: r.content[0]?.text ?? "",
      })),
      totalResults: result.totalResults,
    };

    // Create artifact for display in ExecutionSummaryBar
    const artifact: Artifact = {
      id: `file-search-${toolCall.id}`,
      type: "file_search",
      title: "Knowledge Base Search",
      role: "output",
      toolCallId: toolCall.id,
      data: artifactData,
    };

    return {
      success: true,
      output: result.content,
      citations,
      artifacts: [artifact],
    };
  } else {
    return {
      success: false,
      error: result.error,
      output: result.content, // Error content formatted for the model
    };
  }
};

/**
 * Code interpreter tool call arguments
 */
interface CodeInterpreterArguments {
  code: string;
  packages?: string[];
}

/**
 * Execute the code_interpreter tool using Pyodide
 *
 * Runs Python code in-browser using Pyodide (Python compiled to WebAssembly).
 * Captures stdout/stderr and any matplotlib figures as artifacts.
 */
export const codeInterpreterExecutor: ToolExecutor = async (
  toolCall,
  context
): Promise<ToolExecutionResult> => {
  if (toolCall.name !== "code_interpreter") {
    return {
      success: false,
      error: `Expected code_interpreter tool, got ${toolCall.name}`,
    };
  }

  const args = toolCall.arguments as unknown as CodeInterpreterArguments;
  const { code, packages } = args;

  if (!code || typeof code !== "string") {
    return {
      success: false,
      error: "No code provided to code_interpreter",
      output: JSON.stringify({ error: "No code provided" }),
    };
  }

  // Generate a unique ID for artifacts if toolCall.id is missing
  // (can happen with some OpenAI-compatible providers that don't include item.id)
  const toolId = toolCall.id || `code-${Date.now()}-${Math.random().toString(36).slice(2, 9)}`;

  // Subscribe to Pyodide status updates if callback is provided
  let unsubscribe: (() => void) | undefined;
  if (context.onStatusMessage) {
    // Check current status and emit initial message
    const status = pyodideService.getStatus();
    if (status === "loading") {
      context.onStatusMessage(toolCall.id, "Loading Python runtime...");
    } else if (status === "idle") {
      context.onStatusMessage(toolCall.id, "Initializing Python...");
    } else if (status === "ready") {
      context.onStatusMessage(toolCall.id, "Executing code...");
    }

    // Subscribe to status changes
    unsubscribe = pyodideService.onStatusChange((status, message) => {
      if (status === "loading") {
        context.onStatusMessage?.(toolCall.id, message || "Loading Python runtime...");
      } else if (status === "ready") {
        context.onStatusMessage?.(toolCall.id, "Executing code...");
      }
    });
  }

  try {
    // Execute Python code using Pyodide service
    const result = await pyodideService.execute(code, {
      packages,
      signal: context.signal,
      timeout: 60000, // 60 second timeout for code execution
    });

    // Unsubscribe from status updates
    unsubscribe?.();

    // Build artifacts from execution results
    const artifacts: Artifact[] = [];
    let artifactIndex = 0;

    // Always add the executed code as the first artifact so users can see what ran
    artifacts.push({
      id: `code-input-${toolId}-${artifactIndex++}`,
      type: "code",
      title: "Python",
      role: "input",
      toolCallId: toolId,
      data: {
        language: "python",
        code: code,
      },
    });

    // Add code output artifact if there's stdout
    if (result.stdout) {
      artifacts.push({
        id: `code-output-${toolId}-${artifactIndex++}`,
        type: "code",
        title: "Output",
        role: "output",
        toolCallId: toolId,
        data: {
          language: "text",
          code: result.stdout,
        },
      });
    }

    // Add stderr as a separate artifact if present
    if (result.stderr) {
      artifacts.push({
        id: `code-stderr-${toolId}-${artifactIndex++}`,
        type: "code",
        title: "Stderr",
        role: "output",
        toolCallId: toolId,
        data: {
          language: "text",
          code: result.stderr,
        },
      });
    }

    // Add matplotlib figures as image artifacts
    for (const figure of result.figures) {
      artifacts.push({
        id: `code-figure-${toolId}-${artifactIndex++}`,
        type: "image",
        title: "Figure",
        role: "output",
        toolCallId: toolId,
        data: `data:image/png;base64,${figure}`,
        mimeType: "image/png",
      });
    }

    // Build output for the model
    const outputParts: string[] = [];
    if (result.stdout) {
      outputParts.push(`Output:\n${result.stdout}`);
    }
    if (result.stderr) {
      outputParts.push(`Stderr:\n${result.stderr}`);
    }
    if (result.result !== undefined) {
      outputParts.push(`Return value: ${JSON.stringify(result.result)}`);
    }
    if (result.figures.length > 0) {
      outputParts.push(`Generated ${result.figures.length} figure(s)`);
    }
    if (result.error) {
      outputParts.push(`Error:\n${result.error}`);
    }

    const output =
      outputParts.length > 0 ? outputParts.join("\n\n") : "Code executed successfully (no output)";

    return {
      success: result.success,
      output: JSON.stringify({ output }),
      error: result.error,
      artifacts,
    };
  } catch (error) {
    // Unsubscribe from status updates on error
    unsubscribe?.();

    const errorMsg = error instanceof Error ? error.message : formatApiError(error);
    // Still show the code that was attempted, plus the error
    const artifacts: Artifact[] = [
      {
        id: `code-input-${toolId}-0`,
        type: "code",
        title: "Python",
        role: "input",
        toolCallId: toolId,
        data: {
          language: "python",
          code: code,
        },
      },
      {
        id: `code-error-${toolId}-1`,
        type: "code",
        title: "Error",
        role: "output",
        toolCallId: toolId,
        data: {
          language: "text",
          code: errorMsg,
        },
      },
    ];
    return {
      success: false,
      error: errorMsg,
      output: JSON.stringify({ error: errorMsg }),
      artifacts,
    };
  }
};

/**
 * JavaScript interpreter tool call arguments
 */
interface JSInterpreterArguments {
  code: string;
}

/**
 * Execute the js_code_interpreter tool using QuickJS
 *
 * Runs JavaScript code in-browser using QuickJS (compiled to WebAssembly).
 * Captures console.log/error/warn output as artifacts.
 */
export const jsInterpreterExecutor: ToolExecutor = async (
  toolCall,
  context
): Promise<ToolExecutionResult> => {
  if (toolCall.name !== "js_code_interpreter") {
    return {
      success: false,
      error: `Expected js_code_interpreter tool, got ${toolCall.name}`,
    };
  }

  const args = toolCall.arguments as unknown as JSInterpreterArguments;
  const { code } = args;

  if (!code || typeof code !== "string") {
    return {
      success: false,
      error: "No code provided to js_code_interpreter",
      output: JSON.stringify({ error: "No code provided" }),
    };
  }

  // Generate a unique ID for artifacts if toolCall.id is missing
  const toolId = toolCall.id || `js-${Date.now()}-${Math.random().toString(36).slice(2, 9)}`;

  // Subscribe to QuickJS status updates if callback is provided
  let unsubscribe: (() => void) | undefined;
  if (context.onStatusMessage) {
    const status = quickjsService.getStatus();
    if (status === "loading") {
      context.onStatusMessage(toolId, "Loading JavaScript runtime...");
    } else if (status === "idle") {
      context.onStatusMessage(toolId, "Initializing JavaScript...");
    } else if (status === "ready") {
      context.onStatusMessage(toolId, "Executing code...");
    }

    unsubscribe = quickjsService.onStatusChange((status, message) => {
      if (status === "loading") {
        context.onStatusMessage?.(toolId, message || "Loading JavaScript runtime...");
      } else if (status === "ready") {
        context.onStatusMessage?.(toolId, "Executing code...");
      }
    });
  }

  try {
    // Execute JavaScript code using QuickJS service
    const result = await quickjsService.execute(code, {
      signal: context.signal,
      timeout: 30000, // 30 second timeout for JS execution
    });

    // Unsubscribe from status updates
    unsubscribe?.();

    // Build artifacts from execution results
    const artifacts: Artifact[] = [];
    let artifactIndex = 0;

    // Always add the executed code as the first artifact so users can see what ran
    artifacts.push({
      id: `js-input-${toolId}-${artifactIndex++}`,
      type: "code",
      title: "JavaScript",
      role: "input",
      toolCallId: toolId,
      data: {
        language: "javascript",
        code: code,
      },
    });

    // Add code output artifact if there's stdout
    if (result.stdout) {
      artifacts.push({
        id: `js-output-${toolId}-${artifactIndex++}`,
        type: "code",
        title: "Output",
        role: "output",
        toolCallId: toolId,
        data: {
          language: "text",
          code: result.stdout,
        },
      });
    }

    // Add stderr as a separate artifact if present
    if (result.stderr) {
      artifacts.push({
        id: `js-stderr-${toolId}-${artifactIndex++}`,
        type: "code",
        title: "Stderr",
        role: "output",
        toolCallId: toolId,
        data: {
          language: "text",
          code: result.stderr,
        },
      });
    }

    // Build output for the model
    const outputParts: string[] = [];
    if (result.stdout) {
      outputParts.push(`Output:\n${result.stdout}`);
    }
    if (result.stderr) {
      outputParts.push(`Stderr:\n${result.stderr}`);
    }
    if (result.result !== undefined) {
      outputParts.push(`Return value: ${JSON.stringify(result.result)}`);
    }
    if (result.error) {
      outputParts.push(`Error:\n${result.error}`);
    }

    const output =
      outputParts.length > 0 ? outputParts.join("\n\n") : "Code executed successfully (no output)";

    return {
      success: result.success,
      output: JSON.stringify({ output }),
      error: result.error,
      artifacts,
    };
  } catch (error) {
    // Unsubscribe from status updates on error
    unsubscribe?.();

    const errorMsg = error instanceof Error ? error.message : formatApiError(error);
    // Still show the code that was attempted, plus the error
    const artifacts: Artifact[] = [
      {
        id: `js-input-${toolId}-0`,
        type: "code",
        title: "JavaScript",
        role: "input",
        toolCallId: toolId,
        data: {
          language: "javascript",
          code: code,
        },
      },
      {
        id: `js-error-${toolId}-1`,
        type: "code",
        title: "Error",
        role: "output",
        toolCallId: toolId,
        data: {
          language: "text",
          code: errorMsg,
        },
      },
    ];
    return {
      success: false,
      error: errorMsg,
      output: JSON.stringify({ error: errorMsg }),
      artifacts,
    };
  }
};

/**
 * Web search executor — calls backend /v1/tools/web-search endpoint
 */
export const webSearchExecutor: ToolExecutor = async (
  toolCall,
  context
): Promise<ToolExecutionResult> => {
  if (toolCall.name !== "web_search") {
    return {
      success: false,
      error: `Expected web_search tool, got ${toolCall.name}`,
    };
  }

  const args = toolCall.arguments as { query: string; max_results?: number };

  context.onStatusMessage?.(toolCall.id, "Searching the web...");

  const resp = await fetch("/api/v1/tools/web-search", {
    method: "POST",
    headers: {
      "Content-Type": "application/json",
      ...(context.token && { Authorization: `Bearer ${context.token}` }),
    },
    body: JSON.stringify({ query: args.query, max_results: args.max_results }),
    signal: context.signal,
  });

  if (!resp.ok) {
    const body = await resp.text().catch(() => "");
    return {
      success: false,
      error: `Web search failed: ${resp.status} ${body}`,
      output: JSON.stringify({ error: `Web search request failed with status ${resp.status}` }),
    };
  }

  const data = (await resp.json()) as {
    results?: Array<{ title: string; url: string; content: string; score?: number }>;
  };

  if (!Array.isArray(data?.results)) {
    return {
      success: false,
      error: "Invalid response: missing results array",
      output: JSON.stringify(data),
    };
  }

  // Build table artifact for results
  const artifacts: Artifact[] = [];
  if (data.results.length > 0) {
    const rows = data.results.map((r) => ({
      title: r.title,
      url: r.url,
      snippet: r.content.substring(0, 200) + (r.content.length > 200 ? "..." : ""),
    }));
    artifacts.push({
      id: `search-results-${toolCall.id}`,
      type: "table",
      title: `Search: ${args.query}`,
      data: {
        columns: [
          { key: "title", label: "Title" },
          { key: "url", label: "URL" },
          { key: "snippet", label: "Snippet" },
        ],
        rows,
      },
      role: "output",
    });
  }

  return {
    success: true,
    output: JSON.stringify(data),
    artifacts,
  };
};

/**
 * Web fetch executor — calls backend /v1/tools/web-fetch endpoint
 */
export const webFetchExecutor: ToolExecutor = async (
  toolCall,
  context
): Promise<ToolExecutionResult> => {
  if (toolCall.name !== "web_fetch") {
    return {
      success: false,
      error: `Expected web_fetch tool, got ${toolCall.name}`,
    };
  }

  const args = toolCall.arguments as { url: string; max_length?: number };

  context.onStatusMessage?.(toolCall.id, "Fetching URL...");

  const resp = await fetch("/api/v1/tools/web-fetch", {
    method: "POST",
    headers: {
      "Content-Type": "application/json",
      ...(context.token && { Authorization: `Bearer ${context.token}` }),
    },
    body: JSON.stringify({ url: args.url, max_length: args.max_length }),
    signal: context.signal,
  });

  if (!resp.ok) {
    const body = await resp.text().catch(() => "");
    return {
      success: false,
      error: `Web fetch failed: ${resp.status} ${body}`,
      output: JSON.stringify({ error: `Web fetch request failed with status ${resp.status}` }),
    };
  }

  const data = (await resp.json()) as {
    url: string;
    content_type: string | null;
    content?: string;
    content_length: number;
  };

  if (typeof data?.content !== "string") {
    return {
      success: false,
      error: "Invalid response: missing content string",
      output: JSON.stringify(data),
    };
  }

  return {
    success: true,
    output: JSON.stringify(data),
  };
};

// =============================================================================
// Wikimedia Rate Limiter
// =============================================================================

/**
 * Simple rate limiter for Wikimedia API requests.
 * Implements a sliding window to stay well under the 200 req/s limit.
 * Shared across Wikipedia and Wikidata to respect Wikimedia infrastructure.
 *
 * @see https://foundation.wikimedia.org/wiki/Policy:Wikimedia_Foundation_API_Usage_Guidelines
 */
class WikimediaRateLimiter {
  private requestTimestamps: number[] = [];
  private readonly maxRequestsPerSecond = 10; // Conservative limit (API allows 200)
  private readonly windowMs = 1000;

  /**
   * Wait if necessary to stay within rate limits.
   * Returns immediately if under limit, otherwise delays.
   */
  async throttle(): Promise<void> {
    const now = Date.now();

    // Remove timestamps outside the window
    this.requestTimestamps = this.requestTimestamps.filter((ts) => now - ts < this.windowMs);

    if (this.requestTimestamps.length >= this.maxRequestsPerSecond) {
      // Calculate delay needed
      const oldestInWindow = this.requestTimestamps[0];
      const delayMs = this.windowMs - (now - oldestInWindow) + 10; // +10ms buffer

      if (delayMs > 0) {
        await new Promise((resolve) => setTimeout(resolve, delayMs));
      }
    }

    // Record this request
    this.requestTimestamps.push(Date.now());
  }
}

/** Shared rate limiter for all Wikimedia API requests */
const wikimediaRateLimiter = new WikimediaRateLimiter();

// =============================================================================
// Wikipedia Tool Executor
// =============================================================================

/**
 * User-Agent string for Wikimedia API requests.
 * Required by Wikimedia Foundation policy.
 * @see https://foundation.wikimedia.org/wiki/Policy:Wikimedia_Foundation_User-Agent_Policy
 */
const WIKIMEDIA_USER_AGENT = "Hadrian-Gateway/1.0 (https://github.com/hadriangateway/hadrian)";

/** Wikipedia content license information */
const WIKIPEDIA_LICENSE = {
  name: "Creative Commons Attribution-ShareAlike 4.0",
  shortName: "CC BY-SA 4.0",
  url: "https://creativecommons.org/licenses/by-sa/4.0/",
  notice:
    "Wikipedia content is licensed under CC BY-SA 4.0. " +
    "If you redistribute this content, you must give appropriate credit and use the same license.",
};

/** Wikidata content license information */
const WIKIDATA_LICENSE = {
  name: "Creative Commons CC0 1.0 Universal Public Domain Dedication",
  shortName: "CC0 1.0",
  url: "https://creativecommons.org/publicdomain/zero/1.0/",
  notice: "Wikidata content is in the public domain under CC0 1.0. No attribution required.",
};

/**
 * Wikipedia tool call arguments
 */
interface WikipediaArguments {
  /** Action to perform: "search" to find articles, "get" to fetch article content */
  action: "search" | "get";
  /** Search query (for action="search") or article title (for action="get") */
  query: string;
  /** Language code (default: "en") */
  language?: string;
  /** Maximum results for search (default: 5, max: 20) */
  limit?: number;
}

/**
 * Wikipedia search result from the REST API
 */
interface WikipediaSearchResult {
  pages: Array<{
    id: number;
    key: string;
    title: string;
    excerpt?: string;
    description?: string;
    thumbnail?: {
      url: string;
      width: number;
      height: number;
    };
  }>;
}

/**
 * Execute the wikipedia tool
 *
 * Searches for Wikipedia articles or fetches article summaries using the
 * Wikimedia REST API. Supports multiple language editions.
 *
 * CORS-enabled endpoints (no proxy required).
 * @see https://www.mediawiki.org/wiki/API:REST_API
 */
export const wikipediaExecutor: ToolExecutor = async (
  toolCall,
  context
): Promise<ToolExecutionResult> => {
  if (toolCall.name !== "wikipedia") {
    return {
      success: false,
      error: `Expected wikipedia tool, got ${toolCall.name}`,
    };
  }

  const args = toolCall.arguments as unknown as WikipediaArguments;
  const { action, query, language = "en", limit = 5 } = args;

  if (!query || typeof query !== "string") {
    return {
      success: false,
      error: "No query provided to wikipedia",
      output: JSON.stringify({ error: "No query provided" }),
    };
  }

  if (!action || (action !== "search" && action !== "get")) {
    return {
      success: false,
      error: 'Invalid action. Must be "search" or "get"',
      output: JSON.stringify({ error: 'Invalid action. Must be "search" or "get"' }),
    };
  }

  const toolId = toolCall.id || `wikipedia-${Date.now()}-${Math.random().toString(36).slice(2, 9)}`;

  // Validate language code (simple alphanumeric check)
  const langCode = /^[a-z]{2,3}(-[a-z]{2,4})?$/i.test(language) ? language : "en";

  // Wikipedia REST API endpoint for search
  const searchBaseUrl = `https://${langCode}.wikipedia.org/w/rest.php/v1`;

  try {
    context.onStatusMessage?.(
      toolId,
      action === "search" ? "Searching Wikipedia..." : "Fetching article..."
    );

    // Rate limit before making request
    await wikimediaRateLimiter.throttle();

    if (action === "search") {
      // Search for articles
      const searchLimit = Math.min(Math.max(1, limit), 20);
      const searchUrl = `${searchBaseUrl}/search/page?q=${encodeURIComponent(query)}&limit=${searchLimit}`;

      const response = await fetch(searchUrl, {
        method: "GET",
        headers: {
          "Api-User-Agent": WIKIMEDIA_USER_AGENT,
        },
        signal: context.signal,
      });

      if (!response.ok) {
        throw new Error(`Wikipedia API error: ${response.status} ${response.statusText}`);
      }

      const data = (await response.json()) as WikipediaSearchResult;
      context.onStatusMessage?.(toolId, "");

      // Format results for the model
      const results = data.pages.map((page) => ({
        title: page.title,
        key: page.key,
        description: page.description || null,
        excerpt: page.excerpt?.replace(/<[^>]*>/g, "") || null, // Strip HTML
      }));

      const output = {
        action: "search",
        query,
        language: langCode,
        results,
        resultCount: results.length,
        license: WIKIPEDIA_LICENSE,
      };

      const artifact: Artifact = {
        id: `wikipedia-search-${toolId}`,
        type: "table",
        title: `Wikipedia Search: "${query}"`,
        role: "output",
        toolCallId: toolId,
        data: {
          columns: [
            { key: "title", label: "Title" },
            { key: "description", label: "Description" },
          ],
          rows: results.map((r) => ({
            title: r.title,
            description: r.description || r.excerpt || "—",
          })),
        },
      };

      return {
        success: true,
        output: JSON.stringify(output),
        artifacts: [artifact],
      };
    } else {
      // Get full article using Parse API
      const title = query.replace(/ /g, "_");
      const apiUrl = `https://${langCode}.wikipedia.org/w/api.php`;

      const parseParams = new URLSearchParams({
        action: "parse",
        page: title,
        prop: "text|wikitext",
        format: "json",
        origin: "*",
      });

      const response = await fetch(`${apiUrl}?${parseParams}`, {
        method: "GET",
        headers: { "Api-User-Agent": WIKIMEDIA_USER_AGENT },
        signal: context.signal,
      });

      if (!response.ok) {
        throw new Error(`Wikipedia API error: ${response.status} ${response.statusText}`);
      }

      const data = await response.json();

      if (data.error) {
        return {
          success: false,
          error: `Article not found: ${query}`,
          output: JSON.stringify({
            error: `Article "${query}" not found on ${langCode}.wikipedia.org`,
            suggestion: "Try searching with action='search' first.",
          }),
        };
      }

      const parsed = data.parse;
      const pageTitle = parsed.title;
      const wikitext = parsed.wikitext?.["*"] || "";
      const html = parsed.text?.["*"] || "";
      const articleUrl = `https://${langCode}.wikipedia.org/wiki/${encodeURIComponent(pageTitle)}`;

      context.onStatusMessage?.(toolId, "");

      // Output wikitext for LLM (contains infobox, sections, references)
      const output = {
        action: "get",
        title: pageTitle,
        wikitext,
        url: articleUrl,
        language: langCode,
        license: WIKIPEDIA_LICENSE,
      };

      // Simple artifact: just show Wikipedia's rendered HTML with fixed links
      const fixedHtml = html
        .replace(/href="\/wiki\//g, `href="https://${langCode}.wikipedia.org/wiki/`)
        .replace(/src="\/\//g, 'src="https://')
        .replace(/href="\/\//g, 'href="https://');

      const artifact: Artifact = {
        id: `wikipedia-article-${toolId}`,
        type: "html",
        title: `Wikipedia: ${pageTitle}`,
        role: "output",
        toolCallId: toolId,
        data: {
          html: `
            <div class="wikipedia-article">
              ${fixedHtml}
              <hr style="margin: 16px 0; border: none; border-top: 1px solid #ddd;" />
              <p style="font-size: 0.8em; color: #666;">
                Source: <a href="${articleUrl}" target="_blank">${langCode}.wikipedia.org</a> |
                License: <a href="${WIKIPEDIA_LICENSE.url}" target="_blank">${WIKIPEDIA_LICENSE.shortName}</a>
              </p>
            </div>
            <style>
              .wikipedia-article { font-family: system-ui, sans-serif; line-height: 1.6; max-width: 800px; }
              .wikipedia-article .mw-editsection { display: none; }
              .wikipedia-article .navbox, .wikipedia-article .sistersitebox { display: none; }
              .wikipedia-article img { max-width: 100%; height: auto; }
              .wikipedia-article table { border-collapse: collapse; }
              .wikipedia-article th, .wikipedia-article td { border: 1px solid #ddd; padding: 4px 8px; }
            </style>
          `,
        },
      };

      return {
        success: true,
        output: JSON.stringify(output),
        artifacts: [artifact],
      };
    }
  } catch (error) {
    context.onStatusMessage?.(toolId, "");

    if (error instanceof Error && error.name === "AbortError") {
      return {
        success: false,
        error: "Wikipedia request was cancelled",
        output: JSON.stringify({ error: "Request cancelled" }),
      };
    }

    const errorMsg = error instanceof Error ? error.message : formatApiError(error);
    return {
      success: false,
      error: errorMsg,
      output: JSON.stringify({ error: `Wikipedia request failed: ${errorMsg}` }),
    };
  }
};

// =============================================================================
// Wikidata Tool Executor
// =============================================================================

/**
 * Wikidata tool call arguments
 */
interface WikidataArguments {
  /** Action to perform: "search" to find entities, "get" to fetch entity data */
  action: "search" | "get";
  /** Search query (for action="search") or entity ID like "Q42" (for action="get") */
  query: string;
  /** Language code for labels/descriptions (default: "en") */
  language?: string;
  /** Maximum results for search (default: 5, max: 20) */
  limit?: number;
  /** Entity type filter for search: "item" or "property" (default: "item") */
  type?: "item" | "property";
}

/**
 * Wikidata search result from the Action API
 */
interface WikidataSearchResult {
  search: Array<{
    id: string;
    label: string;
    description?: string;
    url?: string;
    aliases?: string[];
  }>;
  "search-continue"?: number;
}

/**
 * Wikidata entity data from the Action API
 */
interface WikidataEntityResult {
  entities: Record<
    string,
    {
      id: string;
      type: string;
      labels?: Record<string, { language: string; value: string }>;
      descriptions?: Record<string, { language: string; value: string }>;
      aliases?: Record<string, Array<{ language: string; value: string }>>;
      claims?: Record<string, Array<WikidataClaim>>;
      sitelinks?: Record<string, { site: string; title: string; url?: string }>;
    }
  >;
}

/**
 * Wikidata claim/statement structure
 */
interface WikidataClaim {
  mainsnak: {
    snaktype: string;
    property: string;
    datavalue?: {
      type: string;
      value: unknown;
    };
  };
  rank: string;
  qualifiers?: Record<string, Array<unknown>>;
}

/**
 * Format a Wikidata value for output
 */
function formatWikidataValue(datavalue: { type: string; value: unknown } | undefined): unknown {
  if (!datavalue) return null;

  switch (datavalue.type) {
    case "string":
      return datavalue.value;
    case "wikibase-entityid":
      return (datavalue.value as { id: string }).id;
    case "time":
      return (datavalue.value as { time: string }).time;
    case "quantity":
      return {
        amount: (datavalue.value as { amount: string }).amount,
        unit: (datavalue.value as { unit: string }).unit,
      };
    case "globecoordinate":
      return {
        latitude: (datavalue.value as { latitude: number }).latitude,
        longitude: (datavalue.value as { longitude: number }).longitude,
      };
    case "monolingualtext":
      return {
        text: (datavalue.value as { text: string }).text,
        language: (datavalue.value as { language: string }).language,
      };
    default:
      return datavalue.value;
  }
}

/**
 * Execute the wikidata tool
 *
 * Searches for Wikidata entities or fetches structured entity data using the
 * Wikidata Action API. Returns structured knowledge graph data including
 * labels, descriptions, and claims (properties/statements).
 *
 * CORS-enabled with origin=* parameter.
 * @see https://www.wikidata.org/wiki/Wikidata:Data_access
 */
export const wikidataExecutor: ToolExecutor = async (
  toolCall,
  context
): Promise<ToolExecutionResult> => {
  if (toolCall.name !== "wikidata") {
    return {
      success: false,
      error: `Expected wikidata tool, got ${toolCall.name}`,
    };
  }

  const args = toolCall.arguments as unknown as WikidataArguments;
  const { action, query, language = "en", limit = 5, type = "item" } = args;

  if (!query || typeof query !== "string") {
    return {
      success: false,
      error: "No query provided to wikidata",
      output: JSON.stringify({ error: "No query provided" }),
    };
  }

  if (!action || (action !== "search" && action !== "get")) {
    return {
      success: false,
      error: 'Invalid action. Must be "search" or "get"',
      output: JSON.stringify({ error: 'Invalid action. Must be "search" or "get"' }),
    };
  }

  const toolId = toolCall.id || `wikidata-${Date.now()}-${Math.random().toString(36).slice(2, 9)}`;

  // Validate language code
  const langCode = /^[a-z]{2,3}(-[a-z]{2,4})?$/i.test(language) ? language : "en";

  // Base URL for the Wikidata Action API
  const baseUrl = "https://www.wikidata.org/w/api.php";

  try {
    context.onStatusMessage?.(
      toolId,
      action === "search" ? "Searching Wikidata..." : "Fetching entity..."
    );

    // Rate limit before making request
    await wikimediaRateLimiter.throttle();

    if (action === "search") {
      // Search for entities
      const searchLimit = Math.min(Math.max(1, limit), 20);
      const params = new URLSearchParams({
        action: "wbsearchentities",
        search: query,
        language: langCode,
        uselang: langCode,
        type: type,
        limit: searchLimit.toString(),
        format: "json",
        origin: "*", // Enable CORS
      });

      const response = await fetch(`${baseUrl}?${params}`, {
        method: "GET",
        headers: {
          "Api-User-Agent": WIKIMEDIA_USER_AGENT,
        },
        signal: context.signal,
      });

      if (!response.ok) {
        throw new Error(`Wikidata API error: ${response.status} ${response.statusText}`);
      }

      const data = (await response.json()) as WikidataSearchResult;
      context.onStatusMessage?.(toolId, "");

      // Format results for the model
      const results = data.search.map((entity) => ({
        id: entity.id,
        label: entity.label,
        description: entity.description || null,
        aliases: entity.aliases || [],
      }));

      const output = {
        action: "search",
        query,
        language: langCode,
        type,
        results,
        resultCount: results.length,
        license: WIKIDATA_LICENSE,
      };

      const artifact: Artifact = {
        id: `wikidata-search-${toolId}`,
        type: "table",
        title: `Wikidata Search: "${query}"`,
        role: "output",
        toolCallId: toolId,
        data: {
          columns: [
            { key: "id", label: "ID" },
            { key: "label", label: "Label" },
            { key: "description", label: "Description" },
          ],
          rows: results.map((r) => ({
            id: r.id,
            label: r.label,
            description: r.description || "—",
          })),
        },
      };

      return {
        success: true,
        output: JSON.stringify(output),
        artifacts: [artifact],
      };
    } else {
      // Get entity data by Q-ID or P-ID
      const entityId = query.toUpperCase();
      if (!/^[QP]\d+$/.test(entityId)) {
        return {
          success: false,
          error: `Invalid entity ID: ${query}. Must be a Q-ID (e.g., Q42) or P-ID (e.g., P31)`,
          output: JSON.stringify({
            error: `Invalid entity ID format. Expected Q-ID or P-ID, got: ${query}`,
            suggestion: "Use action='search' first to find the correct entity ID.",
          }),
        };
      }

      const params = new URLSearchParams({
        action: "wbgetentities",
        ids: entityId,
        languages: langCode,
        format: "json",
        origin: "*",
      });

      const response = await fetch(`${baseUrl}?${params}`, {
        method: "GET",
        headers: { "Api-User-Agent": WIKIMEDIA_USER_AGENT },
        signal: context.signal,
      });

      if (!response.ok) {
        throw new Error(`Wikidata API error: ${response.status} ${response.statusText}`);
      }

      const data = (await response.json()) as WikidataEntityResult;
      const entity = data.entities[entityId];

      if (!entity || entity.id === undefined) {
        return {
          success: false,
          error: `Entity not found: ${entityId}`,
          output: JSON.stringify({
            error: `Entity "${entityId}" not found on Wikidata`,
            suggestion: "Use action='search' first.",
          }),
        };
      }

      context.onStatusMessage?.(toolId, "");

      // Extract basic info
      const label = entity.labels?.[langCode]?.value || entity.labels?.en?.value || null;
      const description =
        entity.descriptions?.[langCode]?.value || entity.descriptions?.en?.value || null;
      const aliases = entity.aliases?.[langCode]?.map((a) => a.value) || [];

      // Format claims simply
      const claims: Record<string, unknown[]> = {};
      if (entity.claims) {
        for (const [propId, propClaims] of Object.entries(entity.claims).slice(0, 20)) {
          claims[propId] = propClaims
            .slice(0, 3)
            .map((claim) => formatWikidataValue(claim.mainsnak.datavalue))
            .filter((v) => v !== null);
        }
      }

      // Get Wikipedia links
      const wikipediaLinks: Record<string, string> = {};
      if (entity.sitelinks) {
        for (const [site, link] of Object.entries(entity.sitelinks)) {
          if (site.endsWith("wiki") && !site.includes("quote") && !site.includes("source")) {
            wikipediaLinks[site.replace("wiki", "")] = link.title;
          }
        }
      }

      const entityUrl = `https://www.wikidata.org/wiki/${entityId}`;

      const output = {
        action: "get",
        id: entity.id,
        type: entity.type,
        label,
        description,
        aliases,
        claims,
        wikipediaLinks: Object.keys(wikipediaLinks).length > 0 ? wikipediaLinks : null,
        url: entityUrl,
        language: langCode,
        license: WIKIDATA_LICENSE,
      };

      // Simple artifact with entity summary
      const artifact: Artifact = {
        id: `wikidata-entity-${toolId}`,
        type: "html",
        title: `Wikidata: ${label || entityId}`,
        role: "output",
        toolCallId: toolId,
        data: {
          html: `
            <div style="font-family: system-ui, sans-serif; max-width: 600px; padding: 16px;">
              <h2 style="margin: 0 0 4px 0;">${label || entityId}</h2>
              <p style="color: #666; margin: 0 0 8px 0; font-size: 0.9em;">${entityId}</p>
              ${description ? `<p style="margin: 0 0 12px 0;">${description}</p>` : ""}
              ${aliases.length > 0 ? `<p style="color: #666; font-size: 0.9em; margin: 0 0 12px 0;">Also: ${aliases.join(", ")}</p>` : ""}
              <p style="margin: 12px 0 0 0;">
                <a href="${entityUrl}" target="_blank">View on Wikidata</a>
              </p>
              <hr style="margin: 12px 0; border: none; border-top: 1px solid #ddd;" />
              <p style="font-size: 0.8em; color: #666;">
                License: <a href="${WIKIDATA_LICENSE.url}" target="_blank">${WIKIDATA_LICENSE.shortName}</a>
              </p>
            </div>
          `,
        },
      };

      return {
        success: true,
        output: JSON.stringify(output),
        artifacts: [artifact],
      };
    }
  } catch (error) {
    context.onStatusMessage?.(toolId, "");

    if (error instanceof Error && error.name === "AbortError") {
      return {
        success: false,
        error: "Wikidata request was cancelled",
        output: JSON.stringify({ error: "Request cancelled" }),
      };
    }

    const errorMsg = error instanceof Error ? error.message : formatApiError(error);
    return {
      success: false,
      error: errorMsg,
      output: JSON.stringify({ error: `Wikidata request failed: ${errorMsg}` }),
    };
  }
};

/**
 * HTML render tool call arguments
 */
interface HtmlRenderArguments {
  html: string;
  title?: string;
}

/**
 * Execute the html_render tool
 *
 * Takes HTML content and returns it as an HTML artifact for sandboxed preview.
 * The HTML is rendered client-side in a sandboxed iframe with restricted permissions.
 */
export const htmlRenderExecutor: ToolExecutor = async (
  toolCall,
  _context
): Promise<ToolExecutionResult> => {
  if (toolCall.name !== "html_render") {
    return {
      success: false,
      error: `Expected html_render tool, got ${toolCall.name}`,
    };
  }

  const args = toolCall.arguments as unknown as HtmlRenderArguments;
  const { html, title } = args;

  if (!html || typeof html !== "string") {
    return {
      success: false,
      error: "No HTML content provided to html_render",
      output: JSON.stringify({ error: "No HTML content provided" }),
    };
  }

  // Generate a unique ID for the artifact
  const toolId = toolCall.id || `html-${Date.now()}-${Math.random().toString(36).slice(2, 9)}`;

  // Create the HTML artifact
  const artifact: Artifact = {
    id: `html-${toolId}`,
    type: "html",
    title: title || "HTML Preview",
    role: "output",
    toolCallId: toolId,
    data: { html },
  };

  return {
    success: true,
    output: JSON.stringify({
      message: "HTML rendered successfully",
      contentLength: html.length,
    }),
    artifacts: [artifact],
  };
};

/**
 * Chart render tool call arguments
 */
interface ChartRenderArguments {
  spec: Record<string, unknown>;
  title?: string;
}

/**
 * Execute the chart_render tool
 *
 * Takes a Vega-Lite specification, validates it by compiling, and returns it as
 * a chart artifact for rendering. If the spec is invalid, returns an error so
 * the model can retry with a corrected spec.
 */
export const chartRenderExecutor: ToolExecutor = async (
  toolCall,
  _context
): Promise<ToolExecutionResult> => {
  if (toolCall.name !== "chart_render") {
    return {
      success: false,
      error: `Expected chart_render tool, got ${toolCall.name}`,
    };
  }

  const args = toolCall.arguments as unknown as ChartRenderArguments;
  const { spec, title } = args;

  if (!spec || typeof spec !== "object") {
    return {
      success: false,
      error: "No valid Vega-Lite spec provided to chart_render",
      output: JSON.stringify({ error: "No valid Vega-Lite specification provided" }),
    };
  }

  // Validate the spec by compiling it with vega-lite before reporting success.
  // This catches invalid specs early so the model can retry.
  try {
    const { compile } = await import("vega-lite");
    compile(spec as unknown as Parameters<typeof compile>[0]);
  } catch (err) {
    const message = err instanceof Error ? err.message : formatApiError(err);
    return {
      success: false,
      error: message,
      output: JSON.stringify({ error: `Invalid Vega-Lite spec: ${message}` }),
    };
  }

  // Generate a unique ID for the artifact
  const toolId = toolCall.id || `chart-${Date.now()}-${Math.random().toString(36).slice(2, 9)}`;

  // Create the chart artifact
  const chartData: ChartArtifactData = { spec };
  const artifact: Artifact = {
    id: `chart-${toolId}`,
    type: "chart",
    title: title || (spec.title as string) || "Chart",
    role: "output",
    toolCallId: toolId,
    data: chartData,
  };

  return {
    success: true,
    output: JSON.stringify({
      message: "Chart rendered successfully",
      chartType: spec.mark || "unknown",
    }),
    artifacts: [artifact],
  };
};

/**
 * SQL query tool call arguments
 */
interface SQLQueryArguments {
  sql: string;
}

/**
 * Execute the sql_query tool using DuckDB WASM
 *
 * Runs SQL queries in-browser using DuckDB (compiled to WebAssembly).
 * Supports querying CSV, Parquet, JSON, and SQLite files registered in the virtual filesystem.
 * Returns results as a table artifact for display.
 */
export const sqlQueryExecutor: ToolExecutor = async (
  toolCall,
  context
): Promise<ToolExecutionResult> => {
  if (toolCall.name !== "sql_query") {
    return {
      success: false,
      error: `Expected sql_query tool, got ${toolCall.name}`,
    };
  }

  const args = toolCall.arguments as unknown as SQLQueryArguments;
  const { sql } = args;

  if (!sql || typeof sql !== "string") {
    return {
      success: false,
      error: "No SQL query provided to sql_query",
      output: JSON.stringify({ error: "No SQL query provided" }),
    };
  }

  // Generate a unique ID for artifacts if toolCall.id is missing
  const toolId = toolCall.id || `sql-${Date.now()}-${Math.random().toString(36).slice(2, 9)}`;

  // Subscribe to DuckDB status updates if callback is provided
  let unsubscribe: (() => void) | undefined;
  if (context.onStatusMessage) {
    const status = duckdbService.getStatus();
    if (status === "loading") {
      context.onStatusMessage(toolId, "Loading SQL engine...");
    } else if (status === "idle") {
      context.onStatusMessage(toolId, "Initializing DuckDB...");
    } else if (status === "ready") {
      context.onStatusMessage(toolId, "Executing query...");
    }

    unsubscribe = duckdbService.onStatusChange((status, message) => {
      if (status === "loading") {
        context.onStatusMessage?.(toolId, message || "Loading SQL engine...");
      } else if (status === "ready") {
        context.onStatusMessage?.(toolId, "Executing query...");
      }
    });
  }

  try {
    // Execute SQL query using DuckDB service
    const result = await duckdbService.execute(sql, {
      signal: context.signal,
      timeout: 60000, // 60 second timeout for SQL queries
    });

    // Build artifacts from execution results
    const artifacts: Artifact[] = [];
    let artifactIndex = 0;

    // Always add the executed SQL as the first artifact so users can see what ran
    artifacts.push({
      id: `sql-input-${toolId}-${artifactIndex++}`,
      type: "code",
      title: "SQL Query",
      role: "input",
      toolCallId: toolId,
      data: {
        language: "sql",
        code: sql,
      },
    });

    if (result.success && result.rows.length > 0) {
      // Convert DuckDB result to TableArtifactData format
      const tableData: TableArtifactData = {
        columns: result.columns.map((col) => ({
          key: col.name,
          label: col.name,
        })),
        rows: result.rows,
      };

      artifacts.push({
        id: `sql-result-${toolId}-${artifactIndex++}`,
        type: "table",
        title: `Results (${result.rowCount} row${result.rowCount !== 1 ? "s" : ""})`,
        role: "output",
        toolCallId: toolId,
        data: tableData,
      });
    } else if (result.success && result.rows.length === 0) {
      // Query succeeded but returned no rows
      artifacts.push({
        id: `sql-empty-${toolId}-${artifactIndex++}`,
        type: "code",
        title: "Result",
        role: "output",
        toolCallId: toolId,
        data: {
          language: "text",
          code: "Query executed successfully. No rows returned.",
        },
      });
    }

    // Build output for the model
    let output: string;
    if (result.success) {
      if (result.rows.length > 0) {
        // Format results as a simple text table for the model
        const columnNames = result.columns.map((c) => c.name);
        const header = columnNames.join(" | ");
        const separator = columnNames.map(() => "---").join(" | ");

        // Limit rows sent to model to prevent context overflow
        const maxRowsForModel = 50;
        const rowsToSend = result.rows.slice(0, maxRowsForModel);
        const rowStrings = rowsToSend.map((row) =>
          columnNames.map((col) => String(row[col] ?? "NULL")).join(" | ")
        );

        const tableText = [header, separator, ...rowStrings].join("\n");
        const truncationNote =
          result.rowCount > maxRowsForModel
            ? `\n\n(Showing first ${maxRowsForModel} of ${result.rowCount} rows)`
            : "";

        output = `Query returned ${result.rowCount} row(s):\n\n${tableText}${truncationNote}`;
      } else {
        output = "Query executed successfully. No rows returned.";
      }
    } else {
      output = `SQL Error: ${result.error}`;
      // Add error artifact
      artifacts.push({
        id: `sql-error-${toolId}-${artifactIndex++}`,
        type: "code",
        title: "Error",
        role: "output",
        toolCallId: toolId,
        data: {
          language: "text",
          code: result.error || "Unknown error",
        },
      });
    }

    // Unsubscribe from status updates
    unsubscribe?.();

    return {
      success: result.success,
      output: JSON.stringify({ output }),
      error: result.error,
      artifacts,
    };
  } catch (error) {
    // Unsubscribe from status updates on error
    unsubscribe?.();

    const errorMsg = error instanceof Error ? error.message : formatApiError(error);
    // Still show the SQL that was attempted, plus the error
    const artifacts: Artifact[] = [
      {
        id: `sql-input-${toolId}-0`,
        type: "code",
        title: "SQL Query",
        role: "input",
        toolCallId: toolId,
        data: {
          language: "sql",
          code: sql,
        },
      },
      {
        id: `sql-error-${toolId}-1`,
        type: "code",
        title: "Error",
        role: "output",
        toolCallId: toolId,
        data: {
          language: "text",
          code: errorMsg,
        },
      },
    ];
    return {
      success: false,
      error: errorMsg,
      output: JSON.stringify({ error: errorMsg }),
      artifacts,
    };
  }
};

/**
 * Display artifacts tool call arguments
 */
interface DisplayArtifactsArguments {
  artifacts: string[];
  layout?: "inline" | "gallery" | "stacked";
}

/**
 * Execute the display_artifacts tool
 *
 * This is a meta-tool that doesn't execute anything - it simply indicates
 * which artifacts should be displayed prominently to the user.
 * The model calls this after other tools have executed to select which
 * outputs are most relevant to show inline.
 *
 * The tool produces a special "display_selection" artifact that the UI
 * uses to determine rendering (inline vs collapsed).
 */
export const displayArtifactsExecutor: ToolExecutor = async (
  toolCall,
  _context
): Promise<ToolExecutionResult> => {
  if (toolCall.name !== "display_artifacts") {
    return {
      success: false,
      error: `Expected display_artifacts tool, got ${toolCall.name}`,
    };
  }

  const args = toolCall.arguments as unknown as DisplayArtifactsArguments;
  const { artifacts: artifactIds, layout = "inline" } = args;

  if (!artifactIds || !Array.isArray(artifactIds)) {
    return {
      success: false,
      error: "No artifact IDs provided to display_artifacts",
      output: JSON.stringify({ error: "No artifact IDs provided" }),
    };
  }

  // Generate a unique ID for the selection artifact
  const toolId = toolCall.id || `display-${Date.now()}-${Math.random().toString(36).slice(2, 9)}`;

  // Create the display selection artifact
  const selectionData: DisplaySelectionData = {
    artifactIds,
    layout,
  };

  const selectionArtifact: Artifact = {
    id: `display-selection-${toolId}`,
    type: "display_selection",
    title: "Display Selection",
    role: "output",
    toolCallId: toolId,
    data: selectionData,
  };

  return {
    success: true,
    output: JSON.stringify({
      message: `Selected ${artifactIds.length} artifact(s) for display`,
      artifacts: artifactIds,
      layout,
    }),
    artifacts: [selectionArtifact],
  };
};

/**
 * Sub-agent tool call arguments
 */
interface SubAgentArguments {
  /** Description of the task for the sub-agent to investigate */
  task: string;
}

/**
 * Response structure from the Responses API (non-streaming)
 */
interface ResponsesAPIResult {
  output_text?: string;
  output?: Array<{
    type?: string;
    role?: string;
    content?: Array<{ type?: string; text?: string }>;
  }>;
  usage?: {
    input_tokens: number;
    output_tokens: number;
    total_tokens: number;
    cost?: number;
    input_tokens_details?: { cached_tokens?: number };
    output_tokens_details?: { reasoning_tokens?: number };
  };
}

/**
 * Extract response text from a Responses API result
 */
function extractResponseText(result: ResponsesAPIResult): string {
  let text = result.output_text || "";

  // If output_text not present, try to extract from output array
  if (!text && result.output) {
    for (const outputItem of result.output) {
      if (outputItem.type === "message" && outputItem.content) {
        const textContent = outputItem.content.find((c) => c.type === "output_text");
        if (textContent?.text) {
          text = textContent.text;
          break;
        }
      }
    }
  }

  return text;
}

/**
 * Execute the sub_agent tool
 *
 * Delegates an investigative task to a separate AI agent using a two-phase approach:
 *
 * Phase 1 (Investigation): The agent thoroughly investigates the task, producing
 * internal reasoning and findings. This is visible to the user but NOT sent to
 * the parent model.
 *
 * Phase 2 (Output): The agent summarizes its findings into a curated output
 * specifically for the parent model. Only this output is returned.
 *
 * Key characteristics:
 * - No tool access (research only)
 * - Isolated context (only sees the task prompt)
 * - Two-phase execution (investigate → summarize)
 * - Parent model only sees curated output, not internal reasoning
 *
 * Use cases:
 * - Breaking down complex research into focused subtasks
 * - Reducing main context size by delegating investigations
 * - Getting a "second opinion" from another model
 */
export const subAgentExecutor: ToolExecutor = async (
  toolCall,
  context
): Promise<ToolExecutionResult> => {
  if (toolCall.name !== "sub_agent") {
    return {
      success: false,
      error: `Expected sub_agent tool, got ${toolCall.name}`,
    };
  }

  const args = toolCall.arguments as unknown as SubAgentArguments;
  const { task } = args;

  if (!task || typeof task !== "string") {
    return {
      success: false,
      error: "No task provided to sub_agent",
      output: JSON.stringify({ error: "No task description provided" }),
    };
  }

  // Use the user-configured default model (set via ToolsBar UI)
  const model = context.defaultModel;
  if (!model) {
    return {
      success: false,
      error: "No sub-agent model configured",
      output: JSON.stringify({
        error:
          "No model configured for sub-agent. Configure a default model in the sub-agent tool settings.",
      }),
    };
  }

  const toolId = toolCall.id || `subagent-${Date.now()}-${Math.random().toString(36).slice(2, 9)}`;

  // Build headers - include Authorization only if token is available
  // Auth may also work via cookies (for OIDC/session auth)
  const headers: Record<string, string> = {
    "Content-Type": "application/json",
  };
  if (context.token) {
    headers.Authorization = `Bearer ${context.token}`;
  }

  // Track combined usage across both phases
  const totalUsage = {
    inputTokens: 0,
    outputTokens: 0,
    totalTokens: 0,
    cost: 0,
  };

  const addUsage = (usage: ResponsesAPIResult["usage"]) => {
    if (usage) {
      totalUsage.inputTokens += usage.input_tokens;
      totalUsage.outputTokens += usage.output_tokens;
      totalUsage.totalTokens += usage.total_tokens;
      totalUsage.cost += usage.cost ?? 0;
    }
  };

  try {
    // ========== PHASE 1: Investigation ==========
    context.onStatusMessage?.(toolId, `Investigating...`);

    const investigationPrompt =
      "You are a research assistant. Thoroughly investigate the following task. " +
      "Think through the problem, consider different angles, and develop your findings. " +
      "Be comprehensive in your analysis.";

    const phase1Response = await fetch("/api/v1/responses", {
      method: "POST",
      headers,
      credentials: "include", // Send cookies for session-based auth
      body: JSON.stringify({
        model,
        input: [
          { role: "system", content: investigationPrompt },
          { role: "user", content: task },
        ],
        stream: false,
        temperature: 0.7,
        max_output_tokens: 4096,
      }),
      signal: context.signal,
    });

    if (!phase1Response.ok) {
      const errorText = await phase1Response.text();
      throw new Error(errorText || phase1Response.statusText);
    }

    const phase1Result = (await phase1Response.json()) as ResponsesAPIResult;
    const internalReasoning = extractResponseText(phase1Result);
    addUsage(phase1Result.usage);

    if (!internalReasoning) {
      throw new Error("Sub-agent investigation returned empty response");
    }

    // ========== PHASE 2: Output Curation ==========
    context.onStatusMessage?.(toolId, `Summarizing findings...`);

    const outputPrompt =
      "Based on your investigation above, provide a clear and concise summary of your key findings. " +
      "Focus only on what would be most useful for the parent model to know. " +
      "Be direct and actionable. Do not include your reasoning process, only the conclusions and relevant details.";

    const phase2Response = await fetch("/api/v1/responses", {
      method: "POST",
      headers,
      credentials: "include", // Send cookies for session-based auth
      body: JSON.stringify({
        model,
        input: [
          { role: "system", content: investigationPrompt },
          { role: "user", content: task },
          { role: "assistant", content: internalReasoning },
          { role: "user", content: outputPrompt },
        ],
        stream: false,
        temperature: 0.5, // Lower temperature for more focused output
        max_output_tokens: 2048,
      }),
      signal: context.signal,
    });

    if (!phase2Response.ok) {
      const errorText = await phase2Response.text();
      throw new Error(errorText || phase2Response.statusText);
    }

    const phase2Result = (await phase2Response.json()) as ResponsesAPIResult;
    const curatedOutput = extractResponseText(phase2Result);
    addUsage(phase2Result.usage);

    if (!curatedOutput) {
      throw new Error("Sub-agent output curation returned empty response");
    }

    // Clear status message
    context.onStatusMessage?.(toolId, "");

    // Build usage data (only include if we have actual usage)
    const usageData =
      totalUsage.totalTokens > 0
        ? {
            inputTokens: totalUsage.inputTokens,
            outputTokens: totalUsage.outputTokens,
            totalTokens: totalUsage.totalTokens,
            cost: totalUsage.cost > 0 ? totalUsage.cost : undefined,
          }
        : undefined;

    // Build output for the parent model (only the curated output)
    const output = {
      task,
      model,
      output: curatedOutput,
    };

    // Create agent artifact for UI display (includes internal reasoning for user)
    const agentArtifact: Artifact = {
      id: `agent-${toolId}`,
      type: "agent",
      title: `Sub-Agent (${model.split("/").pop()})`,
      role: "output",
      toolCallId: toolId,
      data: {
        task,
        model,
        internal: internalReasoning,
        output: curatedOutput,
        usage: usageData,
      },
    };

    return {
      success: true,
      output: JSON.stringify(output),
      artifacts: [agentArtifact],
    };
  } catch (error) {
    // Clear status message on error
    context.onStatusMessage?.(toolId, "");

    const errorMsg = error instanceof Error ? error.message : formatApiError(error);

    // Check for abort
    if (error instanceof Error && error.name === "AbortError") {
      return {
        success: false,
        error: "Sub-agent task was cancelled",
        output: JSON.stringify({ error: "Task cancelled" }),
      };
    }

    return {
      success: false,
      error: errorMsg,
      output: JSON.stringify({
        error: `Sub-agent failed: ${errorMsg}`,
        task,
        model,
      }),
    };
  }
};

// =============================================================================
// MCP Tool Executor
// =============================================================================

/** MCP tool name prefix used for namespacing */
export const MCP_TOOL_PREFIX = "mcp_";

/**
 * Parse an MCP tool name to extract server ID and original tool name.
 * Tool names are formatted as "mcp_{serverId}_{toolName}"
 */
export function parseMCPToolName(name: string): { serverId: string; toolName: string } | null {
  if (!name.startsWith(MCP_TOOL_PREFIX)) return null;

  const withoutPrefix = name.slice(MCP_TOOL_PREFIX.length);
  // Find the first underscore to separate serverId from toolName
  const underscoreIndex = withoutPrefix.indexOf("_");
  if (underscoreIndex === -1) return null;

  return {
    serverId: withoutPrefix.slice(0, underscoreIndex),
    toolName: withoutPrefix.slice(underscoreIndex + 1),
  };
}

/**
 * Create an MCP tool name from server ID and tool name.
 * Format: "mcp_{serverId}_{toolName}"
 */
export function createMCPToolName(serverId: string, toolName: string): string {
  return `${MCP_TOOL_PREFIX}${serverId}_${toolName}`;
}

/**
 * Format MCP tool content array into a string output
 */
function formatMCPContent(content: ToolContent[]): string {
  const parts: string[] = [];

  for (const item of content) {
    if (item.type === "text") {
      parts.push(item.text);
    } else if (item.type === "image") {
      // Image content - include as data URL reference
      parts.push(`[Image: ${item.mimeType}]`);
    } else if (item.type === "resource") {
      // Resource content
      if (item.resource.text) {
        parts.push(item.resource.text);
      } else if (item.resource.blob) {
        parts.push(`[Resource: ${item.resource.uri}]`);
      }
    }
  }

  return parts.join("\n");
}

/**
 * MCP tool executor - routes tool calls to external MCP servers
 *
 * Tool names must be prefixed with "mcp_{serverId}_" to route to the correct server.
 * Example: "mcp_github-server_github_search" routes to server "github-server", tool "github_search"
 */
const mcpToolExecutor: ToolExecutor = async (toolCall, context) => {
  const parsed = parseMCPToolName(toolCall.name);
  if (!parsed) {
    return {
      success: false,
      error: `Invalid MCP tool name format: ${toolCall.name}`,
      output: JSON.stringify({ error: "Tool name must be in format mcp_{serverId}_{toolName}" }),
    };
  }

  const { serverId, toolName } = parsed;
  // Strip the gateway-injected `display` directive before forwarding to the MCP server.
  // The directive is handled downstream by applyInlineDisplay.
  const args = stripDisplayArg(toolCall.arguments as Record<string, unknown> | undefined);
  const toolId = toolCall.id;

  // Report status
  context.onStatusMessage?.(toolCall.id, `Calling ${toolName} on MCP server...`);

  // Build artifacts for timeline display
  const artifacts: Artifact[] = [];
  let artifactIndex = 0;

  // Input artifact: show the arguments sent to the tool
  if (args && Object.keys(args).length > 0) {
    artifacts.push({
      id: `mcp-input-${toolId}-${artifactIndex++}`,
      type: "code",
      title: toolName,
      role: "input",
      toolCallId: toolId,
      data: { language: "json", code: JSON.stringify(args, null, 2) },
    });
  }

  try {
    const result = await callMCPTool(serverId, toolName, args);

    // Clear status message
    context.onStatusMessage?.(toolCall.id, "");

    // Build output artifacts from MCP content
    for (const item of result.content) {
      if (item.type === "text" && item.text) {
        artifacts.push({
          id: `mcp-output-${toolId}-${artifactIndex++}`,
          type: "code",
          title: "Output",
          role: "output",
          toolCallId: toolId,
          data: { language: "text", code: item.text },
        });
      } else if (item.type === "image") {
        artifacts.push({
          id: `mcp-image-${toolId}-${artifactIndex++}`,
          type: "image",
          title: "Image",
          role: "output",
          toolCallId: toolId,
          data: `data:${item.mimeType};base64,${item.data}`,
          mimeType: item.mimeType,
        });
      } else if (item.type === "resource" && item.resource.text) {
        artifacts.push({
          id: `mcp-resource-${toolId}-${artifactIndex++}`,
          type: "code",
          title: item.resource.uri,
          role: "output",
          toolCallId: toolId,
          data: { language: "text", code: item.resource.text },
        });
      }
    }

    // Check for error response
    if (result.isError) {
      const errorOutput = formatMCPContent(result.content);
      return {
        success: false,
        error: errorOutput || "MCP tool returned an error",
        output: JSON.stringify({ error: errorOutput, serverId, toolName }),
        artifacts,
      };
    }

    // Format successful result
    const output = formatMCPContent(result.content);

    return {
      success: true,
      output:
        output || JSON.stringify({ result: "Tool executed successfully", serverId, toolName }),
      artifacts,
    };
  } catch (error) {
    // Clear status message on error
    context.onStatusMessage?.(toolCall.id, "");

    const errorMsg = error instanceof Error ? error.message : formatApiError(error);

    return {
      success: false,
      error: errorMsg,
      output: JSON.stringify({
        error: `MCP tool call failed: ${errorMsg}`,
        serverId,
        toolName,
      }),
      artifacts,
    };
  }
};

/**
 * Default tool executors for all supported tools
 */
export const defaultToolExecutors: ToolExecutorRegistry = {
  file_search: fileSearchExecutor,
  code_interpreter: codeInterpreterExecutor,
  js_code_interpreter: jsInterpreterExecutor,
  sql_query: sqlQueryExecutor,
  chart_render: chartRenderExecutor,
  html_render: htmlRenderExecutor,
  web_search: webSearchExecutor,
  web_fetch: webFetchExecutor,
  display_artifacts: displayArtifactsExecutor,
  sub_agent: subAgentExecutor,
  wikipedia: wikipediaExecutor,
  wikidata: wikidataExecutor,
  Skill: skillExecutor,
};

/**
 * Tool metadata for UI display
 */
export interface ToolMetadata {
  /** Tool identifier (matches executor key) */
  id: string;
  /** Display name */
  name: string;
  /** Description shown in settings */
  description: string;
  /** Lucide icon name */
  icon: string;
  /** Whether the tool is fully implemented */
  implemented: boolean;
  /** Whether this tool requires additional configuration (e.g., vector stores) */
  requiresConfig?: boolean;
  /** Config requirement description */
  configDescription?: string;
  /**
   * Extra guidance appended to the system prompt when this tool is enabled.
   * Use it for UI-specific behavior the tool's own API description can't carry
   * (e.g. "files you write are shown in this chat"). Optional — most tools
   * rely solely on their API-level description.
   */
  systemGuidance?: string;
}

/**
 * Concatenated system-prompt guidance for the enabled tools that define it.
 * Appended after the base system prompt in `useChat`. Empty when no enabled
 * tool contributes guidance.
 */
export function getEnabledToolsSystemGuidance(enabledToolIds: string[]): string {
  const enabled = new Set(enabledToolIds);
  return TOOL_METADATA.filter((t) => enabled.has(t.id) && t.systemGuidance)
    .map((t) => t.systemGuidance!)
    .join("\n\n");
}

/**
 * Full guidance appended after the base system prompt when tools are enabled:
 * the generic agentic block followed by any per-tool `systemGuidance`. Used by
 * both `useChat` (request building) and the settings modal (read-only preview)
 * so the displayed effective prompt matches what is actually sent.
 */
export function getAppendedSystemGuidance(enabledToolIds: string[]): string {
  return [getAgenticGuidance(enabledToolIds), getEnabledToolsSystemGuidance(enabledToolIds)]
    .filter(Boolean)
    .join("\n\n");
}

/**
 * Available tools with metadata for UI display
 */
export const TOOL_METADATA: ToolMetadata[] = [
  {
    id: "agent",
    name: "Agent (Shell)",
    description:
      "Run shell commands in a persistent server-side container. Files persist across turns in /mnt/data, and the whole conversation reuses one container until it expires. Configure the container in this tool's settings.",
    icon: "SquareTerminal",
    implemented: true,
    systemGuidance: `## Shell tool & file output

You have a shell tool that runs commands in a persistent Linux container; \`/mnt/data\` is the working directory and persists for the whole conversation.

Files you save to \`/mnt/data\` are shown to the user directly in this chat: images render inline and other files appear as downloads. So when you produce a visual or downloadable result (a plot, generated image, rendered document, data export, etc.), **save it to \`/mnt/data\`** rather than only printing it or describing it.

The gateway attaches and displays saved files automatically. Refer to a file by name in prose (e.g. "I saved the chart as \`red_square.png\`") and it appears on its own. Markdown image or link syntax pointing at \`/mnt/data/...\` (such as \`![red_square.png](/mnt/data/red_square.png)\`) won't render, because that's a path inside the container rather than a URL the chat can load. Pasting base64-encoded file data won't display either, so just write the file.`,
  },
  {
    id: "file_search",
    name: "File Search",
    description: "Search attached knowledge bases using semantic search",
    icon: "FileSearch",
    implemented: true,
    requiresConfig: true,
    configDescription: "Requires attached vector stores",
  },
  {
    id: "code_interpreter",
    name: "Python Interpreter",
    description:
      "Execute Python code in-browser. Pre-installed: numpy, pandas, scipy, matplotlib, scikit-learn. Auto-installs other PyPI packages.",
    icon: "Terminal",
    implemented: true,
  },
  {
    id: "js_code_interpreter",
    name: "JavaScript Interpreter",
    description: "Execute JavaScript code in a sandboxed environment",
    icon: "Braces",
    implemented: true,
  },
  {
    id: "sql_query",
    name: "SQL Query",
    description: "Execute SQL queries on data files (CSV, Parquet, SQLite) using DuckDB",
    icon: "Sheet",
    implemented: true,
  },
  {
    id: "chart_render",
    name: "Charts",
    description:
      "Create data visualizations using Vega-Lite (bar, line, scatter, pie, etc.). Data must be embedded inline.",
    icon: "BarChart3",
    implemented: true,
  },
  {
    id: "html_render",
    name: "HTML Preview",
    description: "Render HTML content in a sandboxed preview (reports, formatted content, demos)",
    icon: "AppWindow",
    implemented: true,
  },
  {
    id: "web_search",
    name: "Web Search",
    description: "Search the web for current information (requires backend configuration)",
    icon: "Globe",
    implemented: true,
  },
  {
    id: "web_fetch",
    name: "Web Fetch",
    description: "Fetch content from a URL (requires backend configuration)",
    icon: "Download",
    implemented: true,
  },
  {
    id: "wikipedia",
    name: "Wikipedia",
    description:
      "Search and fetch Wikipedia article summaries. Content is community-edited and may contain errors—verify important facts independently. Licensed under CC BY-SA 4.0 (attribution required if redistributed).",
    icon: "BookOpen",
    implemented: true,
  },
  {
    id: "wikidata",
    name: "Wikidata",
    description:
      "Search and fetch structured knowledge from Wikidata. Community-curated data may be incomplete or outdated. Licensed under CC0 (public domain, no attribution required).",
    icon: "Database",
    implemented: true,
  },
  {
    id: "sub_agent",
    name: "Sub-agent",
    description:
      "Delegate a focused subtask to a separate model that runs its own tool loop and reports back. Unlike the Agent (Shell) tool, this spawns a nested LLM with isolated context — it does not run shell commands itself. Best for research and analysis.",
    icon: "Users",
    implemented: true,
  },
  {
    id: "mcp",
    name: "MCP Servers",
    description:
      "Connect to external MCP (Model Context Protocol) servers to access additional tools and data sources",
    icon: "Plug",
    implemented: true,
  },
  {
    id: "display_artifacts",
    name: "Display Artifacts",
    description: "Select which tool outputs to display prominently (auto-enabled with other tools)",
    icon: "LayoutGrid",
    implemented: true,
    // This tool is auto-enabled when other tools produce artifacts
    // It doesn't appear in the settings UI
  },
];

/**
 * Get tool metadata by ID
 */
export function getToolMetadata(toolId: string): ToolMetadata | undefined {
  return TOOL_METADATA.find((t) => t.id === toolId);
}

/**
 * Execute multiple tool calls in parallel
 *
 * @param toolCalls - The tool calls to execute
 * @param context - Execution context (auth, vector stores, etc.)
 * @param executors - Tool executor registry (defaults to defaultToolExecutors)
 * @returns Map of tool call ID to execution result
 */
export async function executeToolCalls(
  toolCalls: ParsedToolCall[],
  context: ToolExecutorContext,
  executors: ToolExecutorRegistry = defaultToolExecutors
): Promise<Map<string, ToolExecutionResult>> {
  const results = new Map<string, ToolExecutionResult>();

  // Execute all tool calls in parallel
  const execPromises = toolCalls.map(async (toolCall) => {
    // The model emitted this call but its arguments couldn't be parsed. Mirror
    // the backend's invalid-argument contract: don't run the underlying tool,
    // feed the parse error back as a function_call_output so the model can
    // self-correct on the next round. (See src/services/server_tools/mod.rs.)
    if (toolCall.invalid) {
      const message = invalidArgumentsText(toolCall.name, toolCall.invalid);
      results.set(toolCall.id, {
        success: false,
        error: message,
        output: JSON.stringify({ error: message }),
      });
      return;
    }

    // Check for MCP tools (dynamically named with "mcp:" prefix)
    let executor = executors[toolCall.name];

    if (!executor && toolCall.name.startsWith(MCP_TOOL_PREFIX)) {
      // Route MCP tools to the MCP executor
      executor = mcpToolExecutor;
    }

    if (!executor) {
      // Unknown tool - return error
      results.set(toolCall.id, {
        success: false,
        error: `Unknown tool: ${toolCall.name}`,
        output: JSON.stringify({
          error: `Tool "${toolCall.name}" is not supported for client-side execution.`,
        }),
      });
      return;
    }

    try {
      const result = await executor(toolCall, context);
      results.set(toolCall.id, applyInlineDisplay(toolCall, result));
    } catch (error) {
      // Handle executor errors gracefully
      const errorMessage = error instanceof Error ? error.message : "Unknown error";
      const errorResult: ToolExecutionResult = {
        success: false,
        error: errorMessage,
        output: JSON.stringify({
          error: `Tool execution failed: ${errorMessage}`,
        }),
      };
      results.set(toolCall.id, applyInlineDisplay(toolCall, errorResult));
    }
  });

  await Promise.all(execPromises);
  return results;
}

/**
 * Build an artifact manifest for inclusion in tool output.
 * This tells the model what artifacts were produced so it can call display_artifacts.
 *
 * @param artifacts - The artifacts produced by tool execution
 * @returns Array of manifest entries describing each artifact
 */
function buildArtifactManifest(
  artifacts: Artifact[]
): Array<{ id: string; type: ArtifactType; title?: string; role?: ArtifactRole }> {
  return artifacts.map((artifact) => ({
    id: artifact.id,
    type: artifact.type,
    title: artifact.title,
    role: artifact.role,
  }));
}

/**
 * Build input items for continuing a conversation after tool execution
 *
 * This creates the proper format for the OpenAI Responses API to continue
 * after tool calls have been executed.
 *
 * When artifacts are produced, appends an artifact manifest to the output
 * so the model can call display_artifacts to select which to show.
 *
 * @param toolCalls - The tool calls that were executed
 * @param results - Map of tool call ID to execution result
 * @returns Array of input items to append to the conversation
 */
export function buildToolResultInputItems(
  toolCalls: ParsedToolCall[],
  results: Map<string, ToolExecutionResult>
): Array<{ type: string; call_id: string; output: string }> {
  return toolCalls.map((toolCall) => {
    const result = results.get(toolCall.id);
    let output = result?.output ?? JSON.stringify({ error: "No result available" });

    // Append artifact manifest if artifacts were produced
    // This tells the model what artifacts are available for display_artifacts
    if (result?.artifacts && result.artifacts.length > 0) {
      // Filter out display_selection artifacts (those are internal)
      const displayableArtifacts = result.artifacts.filter((a) => a.type !== "display_selection");

      if (displayableArtifacts.length > 0) {
        const manifest = buildArtifactManifest(displayableArtifacts);
        try {
          // Try to merge manifest with existing output JSON
          const parsed = JSON.parse(output);
          parsed._artifacts = manifest;
          output = JSON.stringify(parsed);
        } catch {
          // If output isn't valid JSON, append as separate section
          output += `\n\nArtifacts produced:\n${JSON.stringify(manifest, null, 2)}`;
        }
      }
    }

    return {
      type: "function_call_output",
      call_id: toolCall.callId,
      output,
    };
  });
}
