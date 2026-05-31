import { useCallback, useEffect, useRef } from "react";

import { useAuth } from "@/auth";
import { SseParser } from "@/utils/sseParser";
import {
  useStreamingStore,
  useAllStreams,
  useIsStreaming,
  type StreamingResponse,
} from "@/stores/streamingStore";
import {
  useConversationStore,
  useMessages,
  useSelectedInstances,
} from "@/stores/conversationStore";
import { useDebugStore } from "@/stores/debugStore";
import type {
  CompletedRound,
  ConversationMode,
  ModeConfig,
  MessageModeMetadata,
  ModelInstance,
  PerModelSettings,
  Citation,
  ChunkCitation,
  Artifact,
  ToolExecution,
  ToolExecutionRound,
} from "@/components/chat-types";
import type {
  ChatMessage,
  ChatFile,
  HistoryMode,
  MessageUsage,
  ModelResponse,
  ModelSettings,
} from "./types";
import {
  createToolCallTracker,
  parseToolCallFromEvent,
  type ParsedToolCall,
} from "./utils/toolCallParser";
import {
  executeToolCalls,
  buildToolResultInputItems,
  createMCPToolName,
  getAppendedSystemGuidance,
  DISPLAY_PARAMETER_SCHEMA,
  type ToolExecutorContext,
} from "./utils/toolExecutors";
import { buildSkillToolDescription } from "./utils/skillDirectory";
import { loadSkillSeed } from "./utils/skillExecutor";
import type { SkillResource } from "@/api/generated/types.gen";
import { getToolStatusLabel } from "@/components/ToolIcons";
import { useMCPStore } from "@/stores/mcpStore";
import { useChatUIStore } from "@/stores/chatUIStore";
import {
  sendChainedMode,
  sendRoutedMode,
  sendSynthesizedMode,
  sendRefinedMode,
  sendCritiquedMode,
  sendElectedMode,
  sendTournamentMode,
  sendConsensusMode,
  sendDebatedMode,
  sendCouncilMode,
  sendHierarchicalMode,
  sendScattershotMode,
  sendExplainerMode,
  sendConfidenceWeightedMode,
  filterMessagesForModel,
  type ModeContext,
  type ModeResult,
  type ResponsesStreamEvent,
} from "./modes";
import { getDefaultSystemPrompt } from "@/utils/defaultSystemPrompt";

/** Data file info for SQL query context */
interface DataFileInfo {
  name: string;
  /** For flat files (CSV, Parquet, JSON) */
  columns?: Array<{ name: string; type: string }>;
  /** For database files (DuckDB) */
  tables?: Array<{
    tableName: string;
    schemaName: string;
    columns: Array<{ name: string; type: string }>;
  }>;
  /** Database alias for attached databases */
  dbName?: string;
}

interface UseChatOptions {
  models: string[];
  settings?: ModelSettings;
  historyMode?: HistoryMode;
  /** Conversation mode - controls how multiple models interact */
  conversationMode?: ConversationMode;
  /** Mode-specific configuration */
  modeConfig?: ModeConfig;
  /** Per-model settings including reasoning config */
  perModelSettings?: PerModelSettings;
  /** Attached vector store IDs for file_search tool (RAG) */
  vectorStoreIds?: string[];
  /**
   * Enable client-side tool execution for file_search.
   * When true, the frontend detects tool calls in the SSE stream,
   * executes the search API directly, and sends results back to continue.
   * When false (default), the backend middleware handles tool execution.
   */
  clientSideToolExecution?: boolean;
  /**
   * Enabled tool IDs. Only tools in this list will be sent to the model.
   * Each tool may have additional requirements (e.g., file_search needs vectorStoreIds).
   */
  enabledTools?: string[];
  /**
   * Agent mode: attaches the shell tool (+ container) and optional tool
   * search to the request. Built from `chatUIStore` agent state in ChatPage.
   */
  agentConfig?: {
    enabled: boolean;
    /** OpenAI memory string (e.g. "512m"); empty = operator default. */
    memoryLimit: string;
    /** Idle TTL in minutes; null = operator default. */
    expiresAfterMinutes: number | null;
    /** Egress allowlist (comma/newline separated); empty = inherit. */
    allowedDomains: string;
    /** Attach a `tool_search` tool + defer-load gateway MCP tools. */
    toolSearch: boolean;
    /** Tool-search ranker override; "default" omits it (deployment default). */
    toolSearchRanker: "default" | "hybrid" | "semantic" | "lexical";
  };
  /**
   * Skills enabled for this session. Each skill's name + description is
   * listed in the `Skill` tool's description so the model can match user
   * intent against them. Full SKILL.md bodies are loaded on demand via
   * the frontend `Skill` tool executor (see `skillExecutor.ts`).
   */
  enabledSkills?: SkillResource[];
  /**
   * Data files registered with DuckDB for SQL queries.
   * Used to build dynamic tool description with schema information.
   */
  dataFiles?: DataFileInfo[];
  /**
   * Maximum number of tool execution iterations to prevent infinite loops.
   * Defaults to 25.
   */
  maxToolIterations?: number;
  /**
   * Whether to capture raw SSE events for debugging.
   * When enabled, SSE events are stored in debugStore for inspection.
   */
  captureRawSSEEvents?: boolean;
  /**
   * Default model for sub-agent tool.
   * When null/undefined, uses the current streaming model as fallback.
   */
  subAgentModel?: string | null;
  /** Project ID for usage attribution (sent as X-Hadrian-Project header) */
  projectId?: string;
  /** Conversation ID for per-conversation MCP sessions */
  conversationId?: string;
}

/**
 * Build API content from text and optional files.
 * Images are sent as input_image, text files are inlined as text content.
 * Returns a simple string if no files, or a content array for multi-modal input.
 */
function buildApiContent(content: string, files?: ChatFile[]): string | unknown[] {
  // If no files, return simple format
  if (!files || files.length === 0) {
    return content;
  }

  // Separate images from text files
  const imageFiles = files.filter((f) => f.type.startsWith("image/"));
  const textFiles = files.filter((f) => !f.type.startsWith("image/"));

  // Build content array
  const contentParts: unknown[] = [];

  // Add main text content
  if (content) {
    contentParts.push({ type: "input_text", text: content });
  }

  // Add text files as inline text content
  for (const file of textFiles) {
    // Decode base64 content (file.base64 is a data URL like "data:type;base64,...")
    const base64Data = file.base64.split(",")[1] || "";
    let textContent: string;
    try {
      textContent = atob(base64Data);
    } catch {
      textContent = "[Could not decode file content]";
    }
    contentParts.push({
      type: "input_text",
      text: `\n\n--- File: ${file.name} ---\n${textContent}\n--- End of ${file.name} ---`,
    });
  }

  // Add image files
  for (const file of imageFiles) {
    contentParts.push({
      type: "input_image",
      detail: "auto",
      image_url: file.base64,
    });
  }

  return contentParts;
}

/**
 * Convert a ChatMessage to API input format, including any attached files.
 */
function messageToApiInput(msg: ChatMessage): { role: string; content: string | unknown[] } {
  return { role: msg.role, content: buildApiContent(msg.content, msg.files) };
}

/**
 * Prepend an explicitly slash-invoked skill's `SKILL.md` to the outgoing
 * request content so the skill loads deterministically, instead of nudging the
 * model to call the `Skill` tool. The user's displayed message stays clean;
 * only the API content carries the skill block.
 */
function seedSkillIntoApiContent(
  apiContent: string | unknown[],
  name: string,
  text: string
): string | unknown[] {
  const block = `The user invoked the "${name}" skill for this request. Follow its instructions.\n\n<skill name="${name}">\n${text}\n</skill>`;
  if (typeof apiContent === "string") {
    return apiContent ? `${block}\n\n${apiContent}` : block;
  }
  return [{ type: "input_text", text: block }, ...apiContent];
}

interface UseChatReturn {
  messages: ChatMessage[];
  modelResponses: ModelResponse[];
  isStreaming: boolean;
  /** Resolves when the turn fully completes (including multi-round tool execution). */
  sendMessage: (content: string, files: ChatFile[]) => Promise<void>;
  stopStreaming: () => void;
  clearMessages: () => void;
  /** Set messages directly. For functional updates, use the conversation store's actions. */
  setMessages: (messages: ChatMessage[]) => void;
  regenerateResponse: (userMessageId: string, model: string) => void;
  /**
   * Edit a message and re-run the conversation from that point.
   * For user messages: updates content, deletes all subsequent messages, and streams new responses.
   * For assistant messages: updates content only (preserves sibling model responses).
   */
  editAndRerun: (messageId: string, newContent: string) => void;
  /**
   * Resume a gateway MCP tool call that paused for human approval by echoing
   * back an `mcp_approval_response`. Single-model turns only.
   */
  respondToMcpApproval: (
    assistantMessageId: string,
    approvalRequestId: string,
    approve: boolean
  ) => void;
}

/** Default maximum number of tool execution iterations to prevent infinite loops */
const DEFAULT_maxToolIterations = 25;

/** Result from streaming a response, including any tool calls */
interface StreamResponseResult {
  content: string;
  /** Whether any output_text deltas were received (vs reasoning-only fallback) */
  hasOutputText: boolean;
  usage?: MessageUsage;
  reasoningContent?: string;
  /** Per-round reasoning, content, and tool execution for multi-round tool execution */
  completedRounds?: CompletedRound[];
  /** Tool calls detected during streaming (only when clientSideToolExecution is enabled) */
  toolCalls?: ParsedToolCall[];
  /** Tool execution timeline for progressive disclosure UI */
  toolExecutionRounds?: ToolExecutionRound[];
  /** The request body sent to the API (for debugging) */
  requestBody?: Record<string, unknown>;
  /** The response.output array from the completed response (for debugging) */
  responseOutput?: unknown[];
}

/** One entry in a `shell_call_output` item's Hadrian `output_files` manifest. */
interface ShellOutputFile {
  container_id?: string;
  file_id?: string;
  filename?: string;
  content_type?: string;
  bytes?: number;
  source?: string;
}

/**
 * Build `container_file` artifacts from a shell `output_files` manifest. These
 * render as message-level artifacts (images inline, other files as download
 * chips) by lazily fetching the container content endpoint. Staged inputs
 * (`source === "user"`) are skipped. Ids are stable (`cfile-<file_id>`) so the
 * streamed and `response.completed` paths dedupe against each other.
 */
function containerFilesToArtifacts(outputFiles: ShellOutputFile[] | undefined): Artifact[] {
  if (!outputFiles?.length) return [];
  return outputFiles
    .filter((f) => f.container_id && f.file_id && f.source !== "user")
    .map((f) => ({
      id: `cfile-${f.file_id}`,
      type: "container_file" as const,
      role: "output" as const,
      title: f.filename,
      data: {
        containerId: f.container_id!,
        fileId: f.file_id!,
        filename: f.filename ?? f.file_id!,
        contentType: f.content_type,
        bytes: f.bytes,
      },
    }));
}

/** Extract the metadata fields from a streaming response for committing to the conversation store. */
function commitFieldsFromStream(stream: StreamingResponse | undefined) {
  return {
    citations: stream?.citations,
    artifacts: stream?.artifacts,
    toolExecutionRounds: stream?.toolExecutionRounds,
    completedRounds: stream?.completedRounds.length ? stream.completedRounds : undefined,
    pendingMcpApprovals: stream?.mcpApprovals?.length ? stream.mcpApprovals : undefined,
  };
}

export function useChat({
  models,
  settings,
  historyMode = "all",
  conversationMode = "multiple",
  modeConfig,
  perModelSettings,
  vectorStoreIds,
  clientSideToolExecution = false,
  enabledTools = [],
  agentConfig,
  enabledSkills = [],
  dataFiles = [],
  maxToolIterations = DEFAULT_maxToolIterations,
  captureRawSSEEvents = false,
  subAgentModel,
  projectId,
  conversationId,
}: UseChatOptions): UseChatReturn {
  const { token } = useAuth();
  const abortControllersRef = useRef<AbortController[]>([]);
  // Active shell container per conversation, so the whole conversation reuses
  // one container (via `container_reference`) until it expires — then the next
  // turn provisions a fresh one. Keyed by conversation id.
  const activeContainerRef = useRef<Map<string, string>>(new Map());

  // Use zustand stores instead of local state
  const messages = useMessages();
  const selectedInstances = useSelectedInstances();
  const { setMessages, addUserMessage, addAssistantMessages } = useConversationStore();
  // projectId is passed in from ChatPage (via useConversationSync's currentConversation)
  // and used as a ref to ensure the latest value is available at fetch time.
  const projectIdRef = useRef(projectId);
  projectIdRef.current = projectId;
  const conversationIdRef = useRef(conversationId);
  conversationIdRef.current = conversationId;
  // Pull actions through getState() — subscribing to the entire store would
  // re-render this hook on every streaming/debug update.
  const streamingStore = useStreamingStore.getState();
  const debugStore = useDebugStore.getState();
  const modelResponses = useAllStreams();
  const isStreaming = useIsStreaming();

  const stopStreaming = useCallback(() => {
    abortControllersRef.current.forEach((controller) => controller.abort());
    abortControllersRef.current = [];
    streamingStore.stopStreaming();
  }, [streamingStore]);

  // Abort any in-flight streams when the user switches conversations.
  // Without this, an in-progress stream from conversation A would commit its
  // assistant message into conversation B's store after the switch.
  // Per-send epoch checks below also drop any results that race the abort.
  // Skip the undefined → new-id transition: that's a brand-new conversation
  // being created mid-send, and aborting would kill the very stream we just
  // kicked off.
  const previousConversationIdRef = useRef(conversationId);
  useEffect(() => {
    const previous = previousConversationIdRef.current;
    if (previous === conversationId) return;
    previousConversationIdRef.current = conversationId;
    if (previous === undefined) return;
    abortControllersRef.current.forEach((controller) => controller.abort());
    abortControllersRef.current = [];
    streamingStore.stopStreaming();
    streamingStore.clearStreams();
  }, [conversationId, streamingStore]);

  /**
   * Stream a response from a model using the Responses API
   *
   * @param model - The model ID to use for the API call
   * @param inputItems - The conversation input items
   * @param abortController - Controller for cancellation
   * @param modelSettings - Optional model settings (temperature, etc.)
   * @param streamId - Optional stream ID for the streaming store (defaults to model). Use instance ID for multi-instance support.
   * @param trackToolCalls - Whether to track tool calls for client-side execution
   * @param onSSEEvent - Optional callback for capturing SSE events (for debugging)
   * @param instanceParams - Optional instance-specific parameters (overrides perModelSettings lookup)
   * @returns The response content, usage, reasoning, and any tool calls
   */
  const streamResponse = useCallback(
    async (
      model: string,
      inputItems: Array<{
        role?: string;
        type?: string;
        content?: string | unknown[];
        [key: string]: unknown;
      }>,
      abortController: AbortController,
      modelSettings?: ModelSettings,
      streamId?: string,
      trackToolCalls?: boolean,
      /** Optional callback for capturing SSE events (for debugging) */
      onSSEEvent?: (event: { type: string; timestamp: number; data: unknown }) => void,
      /** Optional instance-specific parameters (overrides perModelSettings lookup) */
      instanceParams?: ModelInstance["parameters"],
      /** Optional instance label for system prompt identity */
      instanceLabel?: string
    ): Promise<StreamResponseResult | null> => {
      // Use streamId for streaming store updates if provided, otherwise use model
      const storeKey = streamId ?? model;

      // Create tool call tracker if client-side tool execution is enabled
      const toolTracker = trackToolCalls ? createToolCallTracker() : null;

      try {
        // Build Responses API input from chat messages
        // Support both role-based messages and type-based items (function_call_output)
        const input = inputItems.map((msg) => {
          if (msg.type) {
            // Type-based input item (e.g., function_call_output) - pass through as-is
            return msg;
          }
          // Role-based message
          return {
            role: msg.role,
            content: typeof msg.content === "string" ? msg.content : msg.content,
          };
        });

        // Per-model settings for this model (instance params override stored per-model settings)
        const perModel = instanceParams ?? perModelSettings?.[model];

        // Add system prompt if not already present in input
        // Some modes (explainer, synthesized, etc.) inject their own specialized system prompts
        // Priority: existing in input > instanceParams > perModelSettings > global modelSettings > default
        const hasSystemMessage = input.some((item) => item.role === "system");
        if (!hasSystemMessage) {
          const basePrompt =
            perModel?.systemPrompt ??
            modelSettings?.systemPrompt ??
            getDefaultSystemPrompt(instanceLabel);
          // Append agentic guidance plus any per-tool system guidance for the
          // enabled tools (e.g. the shell tool explaining that files written to
          // /mnt/data are shown in the chat). Empty when no tools are enabled.
          const toolGuidance = getAppendedSystemGuidance(enabledTools);
          const systemPrompt = toolGuidance ? `${basePrompt}\n\n${toolGuidance}` : basePrompt;
          input.unshift({
            role: "system",
            content: systemPrompt,
          });
        }

        // Build request body with settings
        const requestBody: Record<string, unknown> = {
          model,
          input,
          stream: true,
        };

        // Add optional settings only if explicitly configured
        // Priority: instanceParams > perModelSettings > global modelSettings
        const temperature = perModel?.temperature ?? modelSettings?.temperature;
        if (temperature !== undefined) {
          requestBody.temperature = temperature;
        }
        const maxTokens = perModel?.maxTokens ?? modelSettings?.maxTokens;
        if (maxTokens !== undefined) {
          requestBody.max_output_tokens = maxTokens;
        }
        const topP = perModel?.topP ?? modelSettings?.topP;
        if (topP !== undefined) {
          requestBody.top_p = topP;
        }
        const topK = perModel?.topK ?? modelSettings?.topK;
        if (topK !== undefined) {
          requestBody.top_k = topK;
        }
        const frequencyPenalty = perModel?.frequencyPenalty ?? modelSettings?.frequencyPenalty;
        if (frequencyPenalty !== undefined) {
          requestBody.frequency_penalty = frequencyPenalty;
        }
        const presencePenalty = perModel?.presencePenalty ?? modelSettings?.presencePenalty;
        if (presencePenalty !== undefined) {
          requestBody.presence_penalty = presencePenalty;
        }

        // Add reasoning configuration from per-model settings if enabled
        const reasoning = perModel?.reasoning;
        if (reasoning?.enabled && reasoning.effort !== "none") {
          requestBody.reasoning = {
            effort: reasoning.effort,
          };
        }

        // Build tools array based on enabled tools and their requirements
        const tools: Array<{ type: string; [key: string]: unknown }> = [];

        // Add file_search tool if enabled and vector stores are attached
        if (enabledTools.includes("file_search") && vectorStoreIds && vectorStoreIds.length > 0) {
          tools.push({
            type: "file_search",
            vector_store_ids: vectorStoreIds,
          });
        }

        // Add code_interpreter as a function tool (client-side execution via Pyodide)
        if (enabledTools.includes("code_interpreter")) {
          tools.push({
            type: "function",
            name: "code_interpreter",
            description:
              "Execute Python code in a sandboxed browser environment (Pyodide/WebAssembly). " +
              "Pre-installed: numpy, pandas, scipy, matplotlib, scikit-learn, pillow. " +
              "Additional packages from PyPI are auto-installed when imported (via micropip). " +
              "Use for calculations, data analysis, visualizations, or any Python task. " +
              "Matplotlib figures are automatically captured and displayed. " +
              "Note: Packages with C extensions not compiled for WebAssembly won't work.",
            parameters: {
              type: "object",
              properties: {
                code: {
                  type: "string",
                  description: "The Python code to execute",
                },
                display: DISPLAY_PARAMETER_SCHEMA,
              },
              required: ["code"],
            },
          });
        }

        // Add js_code_interpreter as a function tool (client-side execution via QuickJS)
        if (enabledTools.includes("js_code_interpreter")) {
          tools.push({
            type: "function",
            name: "js_code_interpreter",
            description:
              "Execute JavaScript code in a sandboxed browser environment using QuickJS. " +
              "This is a lightweight, isolated JavaScript runtime with no access to DOM or browser APIs. " +
              "Use console.log() to output results. Supports ES2020 syntax. " +
              "Best for quick calculations, string manipulation, and JSON processing.",
            parameters: {
              type: "object",
              properties: {
                code: {
                  type: "string",
                  description: "The JavaScript code to execute",
                },
                display: DISPLAY_PARAMETER_SCHEMA,
              },
              required: ["code"],
            },
          });
        }

        // Add sql_query as a function tool (client-side execution via DuckDB WASM)
        if (enabledTools.includes("sql_query")) {
          // Build dynamic description with schema information
          let sqlDescription =
            "Execute SQL queries in-browser using DuckDB. " +
            "Supports standard SQL syntax with analytics functions. " +
            "Can query CSV, Parquet, JSON files directly (e.g., SELECT * FROM 'data.csv') " +
            "and DuckDB database files (e.g., SELECT * FROM db_name.table_name). " +
            "Use for data analysis, aggregations, joins, and transformations.";

          // Add available files and their schemas
          if (dataFiles.length > 0) {
            sqlDescription += "\n\nAvailable data:";
            for (const file of dataFiles) {
              if (file.tables && file.tables.length > 0 && file.dbName) {
                // Database file with tables
                for (const table of file.tables) {
                  const columnList = table.columns.map((c) => `${c.name} (${c.type})`).join(", ");
                  sqlDescription += `\n- ${file.dbName}.${table.schemaName}.${table.tableName}: ${columnList}`;
                }
              } else if (file.columns && file.columns.length > 0) {
                const columnList = file.columns.map((c) => `${c.name} (${c.type})`).join(", ");
                sqlDescription += `\n- '${file.name}': ${columnList}`;
              } else if (file.dbName) {
                sqlDescription += `\n- Database '${file.name}' attached as ${file.dbName}`;
              } else {
                sqlDescription += `\n- '${file.name}'`;
              }
            }
          }

          tools.push({
            type: "function",
            name: "sql_query",
            description: sqlDescription,
            parameters: {
              type: "object",
              properties: {
                sql: {
                  type: "string",
                  description: "The SQL query to execute",
                },
                display: DISPLAY_PARAMETER_SCHEMA,
              },
              required: ["sql"],
            },
          });
        }

        // Add chart_render as a function tool (client-side rendering via Vega-Lite)
        if (enabledTools.includes("chart_render")) {
          tools.push({
            type: "function",
            name: "chart_render",
            description:
              "Create data visualizations using Vega-Lite. " +
              "Renders charts in the browser including bar charts, line charts, scatter plots, " +
              "pie/donut charts, area charts, heatmaps, and more. " +
              "Data must be embedded inline in the spec, because the chart renders in the browser with no network access, so external URLs or file references won't load. " +
              'Use the format: {"data": {"values": [{"x": 1, "y": 2}, ...]}, "mark": "...", "encoding": {...}}. ' +
              "If you have data from sql_query or code_interpreter, extract the values and embed them directly. " +
              "Use this when the user asks for charts, graphs, or data visualizations.",
            parameters: {
              type: "object",
              properties: {
                spec: {
                  type: "object",
                  description:
                    "A Vega-Lite specification object. Must include 'data' with inline 'values' array, 'mark', and 'encoding'. " +
                    'Example: {"$schema": "https://vega.github.io/schema/vega-lite/v6.json", ' +
                    '"data": {"values": [{"category": "A", "value": 10}]}, "mark": "bar", ' +
                    '"encoding": {"x": {"field": "category"}, "y": {"field": "value", "type": "quantitative"}}}',
                },
                title: {
                  type: "string",
                  description: "Optional title for the chart (overrides spec.title if provided)",
                },
                display: DISPLAY_PARAMETER_SCHEMA,
              },
              required: ["spec"],
            },
          });
        }

        // Add html_render as a function tool (client-side sandboxed HTML preview)
        if (enabledTools.includes("html_render")) {
          tools.push({
            type: "function",
            name: "html_render",
            description:
              "Render HTML content in a sandboxed preview. " +
              "Use this to display formatted HTML content, reports, interactive demos, or styled output. " +
              "The HTML is rendered in a secure sandboxed iframe with scripts enabled but no external access. " +
              "You can include inline CSS for styling. External resources (images, scripts, stylesheets) will not load. " +
              "Use this when the user asks for formatted output, HTML reports, or web content previews.",
            parameters: {
              type: "object",
              properties: {
                html: {
                  type: "string",
                  description:
                    "The HTML content to render. Can include inline styles and scripts. " +
                    "Should be valid HTML (fragment or complete document).",
                },
                title: {
                  type: "string",
                  description: "Optional title for the preview",
                },
                display: DISPLAY_PARAMETER_SCHEMA,
              },
              required: ["html"],
            },
          });
        }

        // Add display_artifacts tool when any artifact-producing tool is enabled
        // This allows the model to select which outputs to show prominently
        const artifactProducingTools = [
          "code_interpreter",
          "js_code_interpreter",
          "sql_query",
          "chart_render",
          "html_render",
        ];
        const hasArtifactProducingTool = artifactProducingTools.some((t) =>
          enabledTools.includes(t)
        );
        if (hasArtifactProducingTool) {
          tools.push({
            type: "function",
            name: "display_artifacts",
            description:
              "After executing tools that produce outputs (code, charts, tables, images), " +
              "call this to select which artifacts to display prominently to the user inline at this point in the conversation. " +
              "Artifacts not selected will be available in a collapsed 'more outputs' section. " +
              "Call this each time you have outputs to show rather than waiting until the end — " +
              "artifacts appear where you call this function, so call it right after the relevant tools complete. " +
              "Choose the most relevant and interesting outputs - typically final results rather than intermediate steps.",
            parameters: {
              type: "object",
              properties: {
                artifacts: {
                  type: "array",
                  items: { type: "string" },
                  description:
                    "Array of artifact IDs to display prominently, in order of presentation. " +
                    "Artifact IDs are provided in the tool execution results.",
                },
                layout: {
                  type: "string",
                  enum: ["inline", "gallery", "stacked"],
                  description:
                    "How to arrange the displayed artifacts: " +
                    "'inline' (default) - flows with your text response, " +
                    "'gallery' - compact thumbnail grid, " +
                    "'stacked' - full-size vertical stack",
                },
              },
              required: ["artifacts"],
            },
          });
        }

        // Add sub_agent tool for delegating investigative tasks
        if (enabledTools.includes("sub_agent")) {
          tools.push({
            type: "function",
            name: "sub_agent",
            description:
              "Delegate a focused research or analysis task to a separate AI agent. " +
              "The sub-agent runs in isolation with fresh context and no tool access, " +
              "making it ideal for:\n" +
              "- Breaking down complex research into focused subtasks\n" +
              "- Reducing context size by investigating specific aspects separately\n" +
              "- Getting a focused analysis without conversation history baggage\n\n" +
              "Only use for substantial investigative tasks that benefit from isolation. " +
              "For simple questions, answer directly instead.",
            parameters: {
              type: "object",
              properties: {
                task: {
                  type: "string",
                  description:
                    "A clear, detailed description of what to investigate or analyze. " +
                    "Include all necessary context since the sub-agent cannot see the conversation history.",
                },
                display: DISPLAY_PARAMETER_SCHEMA,
              },
              required: ["task"],
            },
          });
        }

        // Add wikipedia tool for searching and fetching Wikipedia articles
        if (enabledTools.includes("wikipedia")) {
          tools.push({
            type: "function",
            name: "wikipedia",
            description:
              "Search Wikipedia articles or fetch article summaries. " +
              "Use action='search' to find articles matching a query. " +
              "Use action='get' to fetch the summary of a specific article by title. " +
              "Supports multiple language editions (en, de, fr, es, etc.).",
            parameters: {
              type: "object",
              properties: {
                action: {
                  type: "string",
                  enum: ["search", "get"],
                  description:
                    "'search' to find articles matching a query, 'get' to fetch a specific article summary",
                },
                query: {
                  type: "string",
                  description:
                    "For action='search': the search query. For action='get': the exact article title (e.g., 'Albert Einstein')",
                },
                language: {
                  type: "string",
                  description:
                    "Language code for Wikipedia edition (default: 'en'). Examples: 'en', 'de', 'fr', 'es', 'ja', 'zh'",
                },
                limit: {
                  type: "number",
                  description: "Maximum number of search results (default: 5, max: 20)",
                },
              },
              required: ["action", "query"],
            },
          });
        }

        // Add wikidata tool for searching and fetching structured data from Wikidata
        if (enabledTools.includes("wikidata")) {
          tools.push({
            type: "function",
            name: "wikidata",
            description:
              "Search and fetch structured data from Wikidata knowledge base. " +
              "Use action='search' to find entities (items or properties) by label. " +
              "Use action='get' to fetch full entity data by Q-ID (e.g., 'Q42' for Douglas Adams) or P-ID (e.g., 'P31' for 'instance of'). " +
              "Returns structured data including labels, descriptions, claims/statements, and Wikipedia links.",
            parameters: {
              type: "object",
              properties: {
                action: {
                  type: "string",
                  enum: ["search", "get"],
                  description:
                    "'search' to find entities by label, 'get' to fetch entity data by ID",
                },
                query: {
                  type: "string",
                  description:
                    "For action='search': the search query. For action='get': the entity ID (e.g., 'Q42', 'P31')",
                },
                language: {
                  type: "string",
                  description:
                    "Language code for labels and descriptions (default: 'en'). Examples: 'en', 'de', 'fr'",
                },
                limit: {
                  type: "number",
                  description: "Maximum number of search results (default: 5, max: 20)",
                },
                type: {
                  type: "string",
                  enum: ["item", "property"],
                  description:
                    "Entity type filter for search (default: 'item'). 'item' for Q-IDs, 'property' for P-IDs",
                },
              },
              required: ["action", "query"],
            },
          });
        }

        // Add web_search tool for searching the live web
        if (enabledTools.includes("web_search")) {
          tools.push({
            type: "function",
            name: "web_search",
            description:
              "Search the web for current or fast-changing information such as recent events, news, prices, release dates, or anything that may have changed since your training data. Returns a ranked list of results with titles, URLs, and short content snippets, not full pages; to read a result in depth, follow up with web_fetch on its URL. Prefer this over answering from memory when freshness matters or you're unsure a fact is current. For information in the user's own documents, use file_search instead.",
            parameters: {
              type: "object",
              properties: {
                query: {
                  type: "string",
                  description: "The search query",
                },
              },
              required: ["query"],
            },
          });
        }

        // Add web_fetch tool for fetching content from a URL
        if (enabledTools.includes("web_fetch")) {
          tools.push({
            type: "function",
            name: "web_fetch",
            description:
              "Fetch the contents of a single web page or API endpoint by URL and return its text. HTML is converted to plain text, so layout, images, and scripts are lost; expect prose, not markup. Use this to read a specific page in depth (often a URL returned by web_search); it cannot reach pages that require login. To discover relevant URLs in the first place, use web_search.",
            parameters: {
              type: "object",
              properties: {
                url: {
                  type: "string",
                  description: "The URL to fetch",
                },
                max_length: {
                  type: "number",
                  description:
                    "Maximum content length in bytes to return (default: server configured limit)",
                },
              },
              required: ["url"],
            },
          });
        }

        // Register the Skill tool when any skills are enabled. A single
        // function whose description lists every available skill's name +
        // description; the model calls `Skill(command)` to load a skill,
        // or `Skill(command, file)` for bundled files. See
        // `buildSkillToolDescription` and `skillExecutor`.
        //
        // Only `disable_model_invocation` gates model access here.
        // `user_invocable: false` is a UI-only flag that hides skills from
        // the slash-command popover — model-only skills (false/false) must
        // still appear in the tool description. A `disable_model_invocation`
        // skill the user slash-invokes is seeded directly into the request
        // (see `seedSkillIntoApiContent`), not surfaced to the model here.
        const invocableSkills = enabledSkills.filter((s) => s.disable_model_invocation !== true);
        if (invocableSkills.length > 0) {
          tools.push({
            type: "function",
            name: "Skill",
            description: buildSkillToolDescription(invocableSkills),
            parameters: {
              type: "object",
              properties: {
                command: {
                  type: "string",
                  description: "Name of the skill to invoke.",
                  enum: invocableSkills.map((s) => s.name),
                },
                file: {
                  type: "string",
                  description:
                    'Optional: relative path of a bundled file to read from the skill (e.g. "scripts/helper.py"). Omit on the first call to receive SKILL.md and the file manifest.',
                },
              },
              required: ["command"],
            },
          });
        }

        // Add MCP tools from configured servers.
        if (enabledTools.includes("mcp")) {
          const allServers = useMCPStore.getState().servers;

          // Gateway servers run server-side: emit a single `mcp` tool object
          // per server and let Hadrian connect + execute. No browser
          // connection is required, so these work for background/agent runs.
          for (const server of allServers) {
            if (!server.enabled || (server.runsOn ?? "browser") !== "gateway") continue;
            const mcpTool: { type: string; [key: string]: unknown } = {
              type: "mcp",
              server_label: server.name || server.id,
              server_url: server.url,
              require_approval: server.requireApproval ?? "never",
            };
            if (server.headers && Object.keys(server.headers).length > 0) {
              mcpTool.headers = server.headers;
            }
            if (server.allowedTools && server.allowedTools.length > 0) {
              mcpTool.allowed_tools = server.allowedTools;
            }
            // With tool search on, defer-load this server's catalog so the
            // model discovers tools lazily instead of loading every definition.
            if (agentConfig?.toolSearch) {
              mcpTool.defer_loading = true;
            }
            tools.push(mcpTool);
          }

          // Browser servers execute client-side: connect, discover, and expose
          // each discovered tool as a `function` tool the client fulfils.
          const hasBrowserServer = allServers.some(
            (s) => s.enabled && (s.runsOn ?? "browser") === "browser"
          );
          if (hasBrowserServer) {
            await useMCPStore.getState().ensureConnected();
          }
          const mcpState = useMCPStore.getState();
          for (const server of mcpState.servers) {
            // Skip disabled, gateway, or disconnected servers
            if (!server.enabled || (server.runsOn ?? "browser") !== "browser") continue;
            if (server.status !== "connected") continue;

            for (const tool of server.tools) {
              // Check if this specific tool is enabled (default to enabled)
              if (server.toolsEnabled[tool.name] === false) continue;

              // Create namespaced tool name to avoid collisions
              const mcpToolName = createMCPToolName(server.id, tool.name);

              // Inject the `display` directive into the schema we show the model.
              // The MCP executor strips it from the arguments before forwarding to the server.
              const baseSchema = tool.inputSchema ?? { type: "object", properties: {} };
              const baseProperties =
                typeof baseSchema === "object" &&
                baseSchema !== null &&
                "properties" in baseSchema &&
                typeof (baseSchema as { properties?: unknown }).properties === "object" &&
                (baseSchema as { properties?: unknown }).properties !== null
                  ? (baseSchema as { properties: Record<string, unknown> }).properties
                  : {};
              const parameters = {
                ...baseSchema,
                type: "object",
                properties: { ...baseProperties, display: DISPLAY_PARAMETER_SCHEMA },
              };

              tools.push({
                type: "function",
                name: mcpToolName,
                description:
                  `[MCP: ${server.name}] ` + (tool.description || `Execute ${tool.name}`),
                parameters,
              });
            }
          }
        }

        // Agent mode: attach the shell tool. Reuse the conversation's existing
        // container if one is live; otherwise provision a fresh one with the
        // configured limits. The captured id is refreshed from each response's
        // terminal event (and cleared on error) below.
        if (agentConfig?.enabled) {
          const conv = conversationIdRef.current;
          const reuseId = conv ? activeContainerRef.current.get(conv) : undefined;
          let environment: Record<string, unknown>;
          if (reuseId) {
            environment = { type: "container_reference", container_id: reuseId };
          } else {
            environment = { type: "container_auto" };
            if (agentConfig.memoryLimit.trim()) {
              environment.memory_limit = agentConfig.memoryLimit.trim();
            }
            if (agentConfig.expiresAfterMinutes && agentConfig.expiresAfterMinutes > 0) {
              environment.expires_after = {
                anchor: "last_active_at",
                minutes: agentConfig.expiresAfterMinutes,
              };
            }
            const allowedDomains = agentConfig.allowedDomains
              .split(/[\n,]/)
              .map((d) => d.trim())
              .filter(Boolean);
            if (allowedDomains.length > 0) {
              environment.network_policy = {
                type: "allowlist",
                allowed_domains: allowedDomains,
              };
            }
          }
          tools.push({ type: "shell", environment });
        }

        // Tool search: let the model search for / lazily load tools.
        if (agentConfig?.toolSearch) {
          const toolSearchTool: { type: string; [key: string]: unknown } = { type: "tool_search" };
          if (agentConfig.toolSearchRanker && agentConfig.toolSearchRanker !== "default") {
            toolSearchTool.ranker = agentConfig.toolSearchRanker;
          }
          tools.push(toolSearchTool);
        }

        // Add tools to request if any are configured
        if (tools.length > 0) {
          requestBody.tools = tools;
        }

        const response = await fetch("/api/v1/responses", {
          method: "POST",
          headers: {
            "Content-Type": "application/json",
            ...(token && { Authorization: `Bearer ${token}` }),
            ...(projectIdRef.current && { "X-Hadrian-Project": projectIdRef.current }),
          },
          body: JSON.stringify(requestBody),
          signal: abortController.signal,
        });

        if (!response.ok) {
          const errorText = await response.text();
          throw new Error(errorText || response.statusText);
        }

        const reader = response.body?.getReader();
        if (!reader) throw new Error("No response body");

        const decoder = new TextDecoder();
        let content = "";
        let reasoningContent = "";
        // Server-side multi-turn tracking. A single `/v1/responses` can carry
        // several model turns (the gateway runs its own tool loop for shell /
        // MCP). Each turn is a distinct reasoning item; when a new reasoning
        // item id appears we flush the prior turn's reasoning+content as a
        // completed round instead of overwriting it. `null` until the first
        // reasoning item is seen.
        let currentReasoningItemId: string | null = null;
        // Server-side tool executions for the current turn, surfaced in the
        // "reasoning & tools" disclosure. Built from the spec items the gateway
        // streams (shell_call/shell_call_output, mcp_call, tool_search) and
        // attached to each completed round so they persist on the message.
        let serverRound = 0;
        let roundExecutions: ToolExecution[] = [];
        const pendingShell = new Map<string, { command: string; startTime: number }>();
        let lastToolSearchQuery = "";
        const codeArtifact = (
          id: string,
          role: "input" | "output",
          language: string,
          code: string,
          title?: string
        ): Artifact => ({ id, type: "code", role, title, data: { language, code } });
        const recordServerExecution = (exec: ToolExecution) => {
          roundExecutions.push(exec);
        };
        const takeRoundExecution = (): ToolExecutionRound | undefined => {
          if (roundExecutions.length === 0) return undefined;
          const round: ToolExecutionRound = {
            round: serverRound,
            executions: roundExecutions,
            hasError: roundExecutions.some((e) => e.status === "error"),
          };
          roundExecutions = [];
          return round;
        };

        const flushServerTurn = (nextItemId?: string) => {
          if (!nextItemId) return;
          if (currentReasoningItemId === null) {
            currentReasoningItemId = nextItemId;
            return;
          }
          if (nextItemId === currentReasoningItemId) return;
          // New reasoning item → previous server turn finished. Commit it as a
          // round so its reasoning isn't overwritten by the next turn.
          const round: CompletedRound = {};
          if (reasoningContent.trim()) round.reasoning = reasoningContent;
          if (content.trim()) round.content = content;
          const toolExecution = takeRoundExecution();
          if (toolExecution) round.toolExecution = toolExecution;
          if (round.reasoning || round.content || round.toolExecution) {
            streamingStore.pushCompletedRound(storeKey, round);
            reasoningContent = "";
            content = "";
            hasOutputText = false;
          }
          serverRound += 1;
          currentReasoningItemId = nextItemId;
        };
        // Spec-compliant SSE parser — handles `\r\n`/`\r`/`\n`, multi-line
        // `data:` fields joined with `\n`, and dispatches events on blank
        // lines instead of every `data:` line.
        const sseParser = new SseParser();
        let usage: MessageUsage | undefined;
        // Fallback: extract tool calls from response.completed if not captured during streaming
        let completedToolCalls: ParsedToolCall[] = [];
        let hasOutputText = false;
        // Capture response output for debugging
        let responseOutput: unknown[] | undefined;

        // Iterate every event yielded by the parser through the existing
        // event-handling logic. We parameterise as a generator so the same
        // body runs for both `feed()` (during streaming) and `flush()` (at
        // end-of-stream).
        const handleEvents = function* (events: Iterable<{ data: string }>) {
          for (const sseEvent of events) {
            const data = sseEvent.data.trim();
            if (!data || data === "[DONE]") continue;
            yield data;
          }
        };

        while (true) {
          const { done, value } = await reader.read();
          if (done) {
            // End of stream: emit any trailing buffered event the producer
            // didn't terminate with a blank line.
            for (const data of handleEvents(sseParser.flush())) {
              await processEventData(data);
            }
            break;
          }

          const chunk = decoder.decode(value, { stream: true });
          for (const data of handleEvents(sseParser.feed(chunk))) {
            await processEventData(data);
          }
        }

        async function processEventData(data: string) {
          try {
            const event = JSON.parse(data) as ResponsesStreamEvent;

            // Capture SSE event for debugging if callback provided
            if (onSSEEvent) {
              onSSEEvent({
                type: event.type,
                timestamp: Date.now(),
                data: event,
              });
            }

            // Track tool calls if enabled
            if (toolTracker) {
              // Cast to BaseSSEEvent since parseToolCallFromEvent expects that type
              const parseResult = parseToolCallFromEvent(
                event as { type: string; [key: string]: unknown },
                toolTracker
              );
              if (parseResult.type === "tool_call_added") {
                // Update streaming store with new tool call
                streamingStore.addToolCall(storeKey, parseResult.toolCall);
              } else if (parseResult.type === "tool_call_arguments_delta") {
                streamingStore.updateToolCallArguments(storeKey, parseResult.id, parseResult.delta);
              } else if (parseResult.type === "tool_call_complete") {
                streamingStore.completeToolCall(
                  storeKey,
                  parseResult.toolCall.id,
                  parseResult.toolCall.arguments as Record<string, unknown>
                );
              }
            }

            // Handle different Responses API event types
            if (event.type === "response.output_text.delta" && event.delta) {
              hasOutputText = true;
              content += event.delta;
              streamingStore.appendContent(storeKey, event.delta);
            } else if (
              (event.type === "response.reasoning_text.delta" ||
                event.type === "response.reasoning_summary_text.delta") &&
              event.delta
            ) {
              // A new reasoning item id means a new server-side turn — commit
              // the prior turn before accumulating this one.
              flushServerTurn(event.item_id);
              // Stream reasoning content (extended thinking)
              reasoningContent += event.delta;
              streamingStore.appendReasoningContent(storeKey, event.delta);
            } else if (
              (event.type === "response.reasoning_text.done" ||
                event.type === "response.reasoning_summary_text.done") &&
              event.text
            ) {
              flushServerTurn(event.item_id);
              // Final reasoning text
              reasoningContent = event.text;
              streamingStore.setReasoningContent(storeKey, reasoningContent);
            } else if (event.type === "response.output_text.done") {
              // Completion signal only — streamed deltas are authoritative.
            } else if (event.type === "response.output_item.done" && event.item) {
              // Handle file_search_call output items (server-side file search)
              if (event.item.type === "file_search_call" && event.item.results) {
                // Convert file_search results to citations
                const citations: Citation[] = event.item.results.map(
                  (
                    result: {
                      file_id: string;
                      filename: string;
                      score: number;
                      content?: Array<{ type: string; text: string }>;
                    },
                    index: number
                  ): ChunkCitation => ({
                    id: `citation-${result.file_id}-${index}`,
                    type: "chunk",
                    fileId: result.file_id,
                    filename: result.filename,
                    score: result.score,
                    chunkIndex: index,
                    content: result.content?.[0]?.text ?? "",
                  })
                );
                if (citations.length > 0) {
                  streamingStore.addCitations(storeKey, citations);
                }
              } else if (event.item.type === "image_generation_call" && event.item.result) {
                // Image generation completed - create image artifact from data URL
                const artifact: Artifact = {
                  id: event.item.id ?? `img_${Date.now()}`,
                  type: "image",
                  title: "Generated Image",
                  data: event.item.result,
                  mimeType: "image/png",
                  role: "output",
                };
                streamingStore.addArtifacts(storeKey, [artifact]);
              } else if (event.item.type === "mcp_list_tools") {
                // Gateway MCP discovered a server's tools.
                const itemId = event.item.id ?? `mcp_list_${Date.now()}`;
                const label = event.item.server_label ?? "mcp";
                const count = event.item.tools?.length ?? 0;
                const ts = Date.now();
                streamingStore.addToolCall(storeKey, {
                  id: itemId,
                  callId: itemId,
                  name: `${label}: list tools`,
                  outputIndex: event.output_index ?? 0,
                  argumentsBuffer: "",
                  status: "pending",
                });
                streamingStore.completeToolCall(storeKey, itemId, { count });
                recordServerExecution({
                  id: itemId,
                  toolName: `${label}: list tools`,
                  status: "success",
                  startTime: ts,
                  endTime: ts,
                  input: { server_label: label },
                  inputArtifacts: [],
                  outputArtifacts: [
                    codeArtifact(
                      `${itemId}-out`,
                      "output",
                      "text",
                      `Loaded ${count} tool${count === 1 ? "" : "s"} from ${label}`
                    ),
                  ],
                  round: serverRound,
                });
              } else if (event.item.type === "mcp_call") {
                // Gateway-executed MCP tool call.
                const itemId = event.item.id ?? `mcp_call_${Date.now()}`;
                const label = event.item.server_label ?? "mcp";
                const name = event.item.name ?? "tool";
                const ts = Date.now();
                streamingStore.addToolCall(storeKey, {
                  id: itemId,
                  callId: itemId,
                  name: `${label}.${name}`,
                  outputIndex: event.output_index ?? 0,
                  argumentsBuffer: event.item.arguments ?? "",
                  status: "pending",
                });
                let parsedArgs: Record<string, unknown> = {};
                try {
                  parsedArgs = event.item.arguments ? JSON.parse(event.item.arguments) : {};
                } catch {
                  // Leave args unparsed; the buffer still renders.
                }
                const errored = !!event.item.error;
                if (errored) {
                  streamingStore.setToolCallError(storeKey, itemId, event.item.error!);
                } else {
                  streamingStore.completeToolCall(storeKey, itemId, parsedArgs);
                }
                recordServerExecution({
                  id: itemId,
                  toolName: `${label}.${name}`,
                  status: errored ? "error" : "success",
                  startTime: ts,
                  endTime: ts,
                  input: parsedArgs,
                  error: event.item.error ?? undefined,
                  inputArtifacts: event.item.arguments?.trim()
                    ? [
                        codeArtifact(
                          `${itemId}-in`,
                          "input",
                          "json",
                          JSON.stringify(parsedArgs, null, 2)
                        ),
                      ]
                    : [],
                  outputArtifacts: event.item.output
                    ? [codeArtifact(`${itemId}-out`, "output", "text", event.item.output)]
                    : [],
                  round: serverRound,
                });
              } else if (
                event.item.type === "mcp_approval_request" &&
                event.item.approval_request_id
              ) {
                // Gateway MCP paused for human approval. Record it on the
                // stream so it commits to the message and renders an
                // approve/deny prompt the user can resume from.
                let parsedArgs: Record<string, unknown> = {};
                try {
                  parsedArgs = event.item.arguments ? JSON.parse(event.item.arguments) : {};
                } catch {
                  // Non-JSON args — render the raw string instead.
                }
                streamingStore.addMcpApproval(storeKey, {
                  id: event.item.id ?? event.item.approval_request_id,
                  approvalRequestId: event.item.approval_request_id,
                  serverLabel: event.item.server_label ?? "mcp",
                  toolName: event.item.name ?? "tool",
                  argumentsJson: event.item.arguments ?? "",
                  parsedArguments: parsedArgs,
                });
              } else if (event.item.type === "shell_call") {
                // Hadrian-hosted shell tool ran a command. Show the command as a
                // live indicator; the paired shell_call_output carries the result.
                const itemId = event.item.id ?? `shell_${Date.now()}`;
                const commands = event.item.action?.commands ?? [];
                streamingStore.addToolCall(storeKey, {
                  id: itemId,
                  callId: itemId,
                  name: "shell",
                  outputIndex: event.output_index ?? 0,
                  argumentsBuffer: commands.join("\n"),
                  status: "pending",
                });
                streamingStore.completeToolCall(storeKey, itemId, { commands });
                const callId = event.item.call_id ?? itemId;
                pendingShell.set(callId, {
                  command: commands.join("\n"),
                  startTime: Date.now(),
                });
              } else if (event.item.type === "shell_call_output") {
                // Result of a shell_call. Pair with the command and record a
                // completed execution carrying the command + stdout/stderr/exit.
                const callId = event.item.call_id ?? event.item.id ?? `shell_${Date.now()}`;
                const pending = pendingShell.get(callId);
                pendingShell.delete(callId);
                const blocks =
                  (
                    event.item as {
                      output?: Array<{
                        stdout?: string;
                        stderr?: string;
                        outcome?: { type?: string; exit_code?: number };
                      }>;
                    }
                  ).output ?? [];
                const stdout = blocks.map((b) => b.stdout ?? "").join("");
                const stderr = blocks.map((b) => b.stderr ?? "").join("");
                const outcome = blocks[0]?.outcome;
                const exitCode = outcome?.exit_code ?? 0;
                const errored = (!!outcome?.type && outcome.type !== "exit") || exitCode !== 0;
                const outText =
                  [stdout, stderr.trim() ? `[stderr]\n${stderr}` : "", `[exit ${exitCode}]`]
                    .filter(Boolean)
                    .join("\n") || "(no output)";
                const now = Date.now();
                // Files the command wrote to /mnt/data surface as prominent
                // message-level artifacts (images inline, others as download
                // chips). Dedup-by-id means the response.completed sweep below
                // is harmless if these also arrive only in the final output.
                const fileArtifacts = containerFilesToArtifacts(
                  (event.item as { output_files?: ShellOutputFile[] }).output_files
                );
                if (fileArtifacts.length > 0) {
                  streamingStore.addArtifacts(storeKey, fileArtifacts);
                }
                recordServerExecution({
                  id: event.item.id ?? `shellout_${now}`,
                  toolName: "shell",
                  status: errored ? "error" : "success",
                  startTime: pending?.startTime ?? now,
                  endTime: now,
                  input: { command: pending?.command ?? "" },
                  inputArtifacts: pending?.command
                    ? [codeArtifact(`${callId}-in`, "input", "bash", pending.command)]
                    : [],
                  outputArtifacts: [codeArtifact(`${callId}-out`, "output", "text", outText)],
                  error: errored ? `exit ${exitCode}` : undefined,
                  round: serverRound,
                });
              } else if (event.item.type === "tool_search_call") {
                // Model searched the deferred tool catalog. Show the query.
                const itemId = event.item.id ?? `ts_${Date.now()}`;
                streamingStore.addToolCall(storeKey, {
                  id: itemId,
                  callId: itemId,
                  name: "tool_search",
                  outputIndex: event.output_index ?? 0,
                  argumentsBuffer: event.item.arguments ?? "",
                  status: "pending",
                });
                let parsedArgs: Record<string, unknown> = {};
                try {
                  parsedArgs = event.item.arguments ? JSON.parse(event.item.arguments) : {};
                } catch {
                  // Leave unparsed; the buffer still renders.
                }
                streamingStore.completeToolCall(storeKey, itemId, parsedArgs);
                lastToolSearchQuery = typeof parsedArgs.query === "string" ? parsedArgs.query : "";
              } else if (event.item.type === "tool_search_output") {
                // Tools were loaded from the search — show the query + how many.
                const itemId = event.item.id ?? `tso_${Date.now()}`;
                const count = event.item.tools?.length ?? 0;
                const now = Date.now();
                streamingStore.addToolCall(storeKey, {
                  id: itemId,
                  callId: itemId,
                  name: "tool_search: loaded tools",
                  outputIndex: event.output_index ?? 0,
                  argumentsBuffer: "",
                  status: "pending",
                });
                streamingStore.completeToolCall(storeKey, itemId, { count });
                recordServerExecution({
                  id: itemId,
                  toolName: "tool_search",
                  status: "success",
                  startTime: now,
                  endTime: now,
                  input: { query: lastToolSearchQuery },
                  inputArtifacts: lastToolSearchQuery
                    ? [codeArtifact(`${itemId}-in`, "input", "text", lastToolSearchQuery)]
                    : [],
                  outputArtifacts: [
                    codeArtifact(
                      `${itemId}-out`,
                      "output",
                      "text",
                      `Loaded ${count} tool${count === 1 ? "" : "s"}`
                    ),
                  ],
                  round: serverRound,
                });
                lastToolSearchQuery = "";
              }
            } else if (event.type === "response.file_search_call.in_progress") {
              // Server-side file search starting - add tool call to streaming store
              const itemId = event.item_id ?? `fs_${Date.now()}`;
              streamingStore.addToolCall(storeKey, {
                id: itemId,
                callId: itemId,
                name: "file_search",
                outputIndex: event.output_index ?? 0,
                argumentsBuffer: "",
                status: "pending",
              });
            } else if (event.type === "response.file_search_call.searching") {
              // Server-side file search actively searching - update status
              if (event.item_id) {
                streamingStore.updateToolCallArguments(storeKey, event.item_id, "");
              }
            } else if (event.type === "response.file_search_call.completed") {
              // Server-side file search completed - remove the tool call indicator
              if (event.item_id) {
                streamingStore.completeToolCall(storeKey, event.item_id, {});
              }
            } else if (event.type === "response.image_generation_call.in_progress") {
              // Image generation starting - show tool call indicator
              const itemId = event.item_id ?? `img_${Date.now()}`;
              streamingStore.addToolCall(storeKey, {
                id: itemId,
                callId: itemId,
                name: "image_generation",
                outputIndex: event.output_index ?? 0,
                argumentsBuffer: "",
                status: "pending",
              });
            } else if (event.type === "response.image_generation_call.generating") {
              // Image generation in progress - update status
              if (event.item_id) {
                streamingStore.updateToolCallArguments(storeKey, event.item_id, "");
              }
            } else if (event.type === "response.image_generation_call.partial_image") {
              // Progressive image preview
              if (event.partial_image_b64) {
                const dataUrl = `data:image/png;base64,${event.partial_image_b64}`;
                const artifact: Artifact = {
                  id: event.item_id ?? `img_partial_${Date.now()}`,
                  type: "image",
                  title: "Generated Image",
                  data: dataUrl,
                  mimeType: "image/png",
                  role: "output",
                };
                streamingStore.setArtifacts(storeKey, [artifact]);
              }
            } else if (event.type === "response.image_generation_call.completed") {
              // Image generation completed - remove tool call indicator
              if (event.item_id) {
                streamingStore.completeToolCall(storeKey, event.item_id, {});
              }
            } else if (event.type === "response.completed" && event.response) {
              // Remember the container this response used so the rest of the
              // conversation reuses it (until it expires and a turn fails — see
              // the catch below, which clears it so the next turn starts fresh).
              const convId = conversationIdRef.current;
              if (event.response.container_id && convId) {
                activeContainerRef.current.set(convId, event.response.container_id);
              }
              // Harvest container files from the final output. Some providers
              // (e.g. OpenRouter-routed models) deliver shell_call_output items
              // only in `response.output` rather than as streamed
              // `output_item.done` events, so the streamed handler above never
              // sees them. Sweeping here makes generated files show regardless;
              // addArtifacts dedupes by id against the streamed path.
              const finalFileArtifacts = (event.response.output ?? [])
                .filter((item) => item.type === "shell_call_output")
                .flatMap((item) =>
                  containerFilesToArtifacts(
                    (item as { output_files?: ShellOutputFile[] }).output_files
                  )
                );
              if (finalFileArtifacts.length > 0) {
                streamingStore.addArtifacts(storeKey, finalFileArtifacts);
              }
              // Extract final text from completed response
              // First try output_text, then message content, then reasoning content as fallback
              const outputText =
                event.response.output_text ||
                event.response.output
                  ?.flatMap(
                    (item) =>
                      item.content
                        ?.filter((c) => c.type === "output_text")
                        .map((c) => c.text || "") ?? []
                  )
                  .join("\n\n---\n\n");

              // If no output_text, try to extract from reasoning content (for reasoning models)
              // This is useful for modes like "elected" where we need to parse a vote number
              // from reasoning-only responses.
              const reasoningText =
                event.response.output
                  ?.filter((item) => item.type === "reasoning")
                  .flatMap((item) => {
                    // Extract from content (reasoning_text items)
                    const fromContent =
                      item.content
                        ?.filter((c) => c.type === "reasoning_text")
                        .map((c) => c.text || "") || [];
                    // Extract from summary (summary_text items)
                    const fromSummary =
                      item.summary
                        ?.filter((s) => s.type === "summary_text")
                        .map((s) => s.text || "") || [];
                    return [...fromContent, ...fromSummary];
                  })
                  .join("") || "";

              // Store reasoning content if present
              if (reasoningText && !reasoningContent) {
                reasoningContent = reasoningText;
                streamingStore.setReasoningContent(storeKey, reasoningContent);
              }

              // Only use response object text as fallback when no streamed deltas were received
              if (!hasOutputText) {
                content = outputText || reasoningText || content;
              }

              // Extract usage data if present
              if (event.response.usage) {
                const u = event.response.usage;
                const completedTime = Date.now();

                // Get timing data from streaming store (use hook.getState() for imperative access)
                const streamState = useStreamingStore.getState().streams.get(storeKey);
                const startTime = streamState?.startTime;
                const firstTokenTime = streamState?.firstTokenTime;

                // Calculate timing stats
                const firstTokenMs =
                  startTime && firstTokenTime ? firstTokenTime - startTime : undefined;
                const totalDurationMs = startTime ? completedTime - startTime : undefined;
                const tokensPerSecond =
                  totalDurationMs && totalDurationMs > 0 && u.output_tokens > 0
                    ? (u.output_tokens / totalDurationMs) * 1000
                    : undefined;

                // Extract provider from model string (format: "provider/model-name")
                const responseModel = event.response.model;
                const provider = responseModel?.includes("/")
                  ? responseModel.split("/")[0]
                  : undefined;

                usage = {
                  inputTokens: u.input_tokens,
                  outputTokens: u.output_tokens,
                  totalTokens: u.total_tokens,
                  cost: u.cost,
                  cachedTokens: u.input_tokens_details?.cached_tokens,
                  reasoningTokens: u.output_tokens_details?.reasoning_tokens,
                  reasoningContent: reasoningContent || undefined,
                  // Timing stats
                  firstTokenMs,
                  totalDurationMs,
                  tokensPerSecond,
                  // Response metadata
                  finishReason: event.response.status,
                  modelId: responseModel,
                  provider,
                };
              }

              // Capture full response output for debugging
              if (event.response.output) {
                responseOutput = event.response.output;
              }

              // Extract function calls from output (fallback for when streaming events don't include them)
              if (trackToolCalls && event.response.output) {
                const functionCalls = event.response.output.filter(
                  (item: { type: string }) => item.type === "function_call"
                ) as Array<{ type: string; call_id: string; name: string; arguments: string }>;
                if (functionCalls.length > 0) {
                  completedToolCalls = functionCalls.map((fc) => ({
                    id: fc.call_id, // Use call_id as id since that's what we have
                    callId: fc.call_id,
                    name: fc.name,
                    status: "completed" as const,
                    arguments: JSON.parse(fc.arguments || "{}"),
                  }));
                }
              }

              // Extract image_generation_call items as fallback
              // (for providers that don't emit output_item.done per item)
              if (event.response.output) {
                const imageItems = event.response.output.filter(
                  (item) => item.type === "image_generation_call" && item.result
                );
                if (imageItems.length > 0) {
                  // Get existing artifact IDs to avoid duplicates
                  const existingArtifacts =
                    useStreamingStore.getState().streams.get(storeKey)?.artifacts ?? [];
                  const existingIds = new Set(existingArtifacts.map((a) => a.id));
                  const newArtifacts: Artifact[] = imageItems
                    .filter((item) => !existingIds.has(item.id ?? ""))
                    .map((item) => ({
                      id: item.id ?? `img_${Date.now()}`,
                      type: "image" as const,
                      title: "Generated Image",
                      data: item.result!,
                      mimeType: "image/png",
                      role: "output" as const,
                    }));
                  if (newArtifacts.length > 0) {
                    streamingStore.addArtifacts(storeKey, newArtifacts);
                  }
                }
              }
            }
          } catch (err) {
            // The SSE parser now joins multi-line `data:` fields and only
            // dispatches on blank lines, so a partial JSON shouldn't reach
            // here. Surface failures at debug so producer/spec drift doesn't
            // silently drop tool calls or citations.
            console.debug("Failed to parse SSE event payload", { data, err });
          }
        }

        // Mark as complete with usage data (include reasoning content in usage)
        if (usage && reasoningContent) {
          usage.reasoningContent = reasoningContent;
        }
        streamingStore.completeStream(storeKey, usage);

        // Get completed tool calls - prefer tracker, fallback to extracted from response.completed
        const trackerToolCalls = toolTracker ? toolTracker.getCompletedToolCalls() : [];
        const toolCalls = trackerToolCalls.length > 0 ? trackerToolCalls : completedToolCalls;

        // The final server turn's tool executions never hit a turn boundary,
        // so hand them to the caller to attach to the last completed round.
        const finalServerRound = takeRoundExecution();

        return {
          content,
          hasOutputText,
          usage,
          reasoningContent: reasoningContent || undefined,
          toolCalls: toolCalls.length > 0 ? toolCalls : undefined,
          toolExecutionRounds: finalServerRound ? [finalServerRound] : undefined,
          requestBody,
          responseOutput,
        };
      } catch (error) {
        if ((error as Error).name === "AbortError") {
          return null;
        }

        const errorMessage = error instanceof Error ? error.message : "Unknown error";
        // Drop the reused container so the next turn provisions a fresh one —
        // an expired/deleted container is the most likely cause of a failure
        // mid-conversation once agent mode is on.
        const convId = conversationIdRef.current;
        if (convId) activeContainerRef.current.delete(convId);
        streamingStore.setError(storeKey, errorMessage);
        return null;
      }
    },
    [
      token,
      streamingStore,
      perModelSettings,
      vectorStoreIds,
      enabledTools,
      agentConfig,
      enabledSkills,
      dataFiles,
    ]
  );

  /**
   * Create a filter function bound to the current history mode
   */
  const createFilterFn = useCallback(
    () => (msgs: ChatMessage[], targetModel: string) =>
      filterMessagesForModel(msgs, targetModel, historyMode),
    [historyMode]
  );

  /**
   * Create mode context for mode handlers
   */
  const createModeContext = useCallback(
    (): ModeContext => ({
      models,
      instances: selectedInstances.length > 0 ? selectedInstances : undefined,
      messages,
      settings,
      modeConfig,
      token: token || "",
      streamingStore,
      abortControllersRef,
      streamResponse,
      filterMessagesForModel: createFilterFn(),
      vectorStoreIds,
    }),
    [
      models,
      selectedInstances,
      messages,
      settings,
      modeConfig,
      token,
      streamingStore,
      streamResponse,
      createFilterFn,
      vectorStoreIds,
    ]
  );

  /**
   * Stream a response with multi-turn tool execution support.
   *
   * When clientSideToolExecution is enabled, this function will:
   * 1. Stream the initial response while tracking tool calls
   * 2. If tool calls are detected, execute them using the tool executor system
   * 3. Send the tool results back to continue the conversation
   * 4. Repeat until no more tool calls or maxToolIterations is reached
   *
   * Also builds a ToolExecutionRound timeline for progressive disclosure UI.
   *
   * @param model - The model ID to use
   * @param initialInputItems - The initial conversation input
   * @param abortController - Controller for cancellation
   * @param modelSettings - Optional model settings
   * @param streamId - Optional stream ID for the streaming store
   * @returns The final response with accumulated content, usage, and execution timeline
   */
  const streamWithToolExecution = useCallback(
    async (
      model: string,
      initialInputItems: Array<{
        role?: string;
        type?: string;
        content?: string | unknown[];
        [key: string]: unknown;
      }>,
      abortController: AbortController,
      modelSettings?: ModelSettings,
      streamId?: string,
      /** Optional message ID for debug capture */
      messageId?: string,
      /** Optional instance-specific parameters (overrides perModelSettings lookup) */
      instanceParams?: ModelInstance["parameters"],
      /** Optional instance label for system prompt identity */
      instanceLabel?: string
    ): Promise<StreamResponseResult | null> => {
      const storeKey = streamId ?? model;

      // Start debug capture if messageId is provided
      if (messageId) {
        debugStore.startDebugCapture(messageId, model);
      }

      // Create SSE event capture callback for debugging
      // This is scoped to track the current round
      let currentDebugRound = 1;
      const createSSECallback = () => {
        if (!messageId || !captureRawSSEEvents) return undefined;
        return (event: { type: string; timestamp: number; data: unknown }) => {
          debugStore.addSSEEvent(messageId, model, currentDebugRound, event);
        };
      };

      // If client-side tool execution is disabled, just use regular streaming
      if (!clientSideToolExecution) {
        const result = await streamResponse(
          model,
          initialInputItems,
          abortController,
          modelSettings,
          streamId,
          false, // trackToolCalls
          createSSECallback(),
          instanceParams,
          instanceLabel
        );
        // Capture single round for debug even without tool execution
        if (messageId && result) {
          debugStore.startDebugRound(messageId, model, 1, initialInputItems);
          if (result.requestBody) {
            debugStore.setRoundRequestBody(messageId, model, 1, result.requestBody);
          }
          if (result.responseOutput) {
            debugStore.setRoundResponseOutput(messageId, model, 1, result.responseOutput);
          }
          debugStore.endDebugRound(messageId, model, 1);
          debugStore.completeDebugCapture(messageId, model, true);
        }
        // Push a completed round so the UI always has completedRounds to render
        // (same as the tool-execution loop does after each iteration)
        if (result) {
          const roundData: CompletedRound = {};
          if (result.reasoningContent) roundData.reasoning = result.reasoningContent;
          if (result.hasOutputText && result.content?.trim()) roundData.content = result.content;
          // Attach the final server turn's tool executions (shell/MCP/tool_search)
          // so they render in the "reasoning & tools" disclosure and persist.
          if (result.toolExecutionRounds?.length) {
            roundData.toolExecution =
              result.toolExecutionRounds[result.toolExecutionRounds.length - 1];
          }
          streamingStore.pushCompletedRound(storeKey, roundData);
          result.completedRounds = [roundData];
        }
        return result;
      }

      let currentInputItems = [...initialInputItems];
      let accumulatedUsage: MessageUsage | undefined;
      let lastReasoningContent: string | undefined;
      const allCompletedRounds: CompletedRound[] = [];
      let iterations = 0;

      // Track execution rounds locally (also mirrored in store for real-time UI)
      const executionRounds: ToolExecutionRound[] = [];

      while (iterations < maxToolIterations) {
        iterations++;
        currentDebugRound = iterations;

        // Start debug round before streaming
        if (messageId) {
          debugStore.startDebugRound(messageId, model, iterations, currentInputItems);
        }

        // Stream response with tool call tracking enabled
        const result = await streamResponse(
          model,
          currentInputItems,
          abortController,
          modelSettings,
          streamId,
          true, // Enable tool call tracking
          createSSECallback(),
          instanceParams,
          instanceLabel
        );

        if (!result) {
          // Aborted or error - complete debug capture
          if (messageId) {
            debugStore.endDebugRound(messageId, model, iterations);
            debugStore.completeDebugCapture(messageId, model, false, "Aborted or error");
          }
          // Aborted or error - return what we have so far
          return iterations === 1
            ? null
            : {
                content: allCompletedRounds
                  .map((r) => r.content)
                  .filter(Boolean)
                  .join("\n\n---\n\n"),
                hasOutputText: true,
                usage: accumulatedUsage,
                reasoningContent: lastReasoningContent,
                completedRounds: allCompletedRounds.length > 0 ? allCompletedRounds : undefined,
                toolExecutionRounds: executionRounds.length > 0 ? executionRounds : undefined,
              };
        }

        // Capture debug data for this round
        if (messageId) {
          if (result.requestBody) {
            debugStore.setRoundRequestBody(messageId, model, iterations, result.requestBody);
          }
          if (result.responseOutput) {
            debugStore.setRoundResponseOutput(messageId, model, iterations, result.responseOutput);
          }
        }

        // Track per-round data for interleaved reasoning/content display.
        // Only count content as meaningful if it has non-whitespace text —
        // models sometimes emit trivial whitespace before tool calls.
        const hasNonTrivialContent = result.hasOutputText && !!result.content?.trim();
        const roundData: CompletedRound = {};
        if (result.reasoningContent) roundData.reasoning = result.reasoningContent;
        if (hasNonTrivialContent) roundData.content = result.content;
        allCompletedRounds.push(roundData);
        // Push to streaming store so the UI can render interleaved rounds during streaming
        streamingStore.pushCompletedRound(storeKey, roundData);
        lastReasoningContent = result.reasoningContent;

        // Accumulate usage (sum tokens across iterations)
        if (result.usage) {
          if (accumulatedUsage) {
            accumulatedUsage = {
              inputTokens: (accumulatedUsage.inputTokens ?? 0) + (result.usage.inputTokens ?? 0),
              outputTokens: (accumulatedUsage.outputTokens ?? 0) + (result.usage.outputTokens ?? 0),
              totalTokens: (accumulatedUsage.totalTokens ?? 0) + (result.usage.totalTokens ?? 0),
              cost: (accumulatedUsage.cost ?? 0) + (result.usage.cost ?? 0),
              cachedTokens: (accumulatedUsage.cachedTokens ?? 0) + (result.usage.cachedTokens ?? 0),
              reasoningTokens:
                (accumulatedUsage.reasoningTokens ?? 0) + (result.usage.reasoningTokens ?? 0),
              reasoningContent: lastReasoningContent,
            };
          } else {
            accumulatedUsage = { ...result.usage };
          }
        }

        // Check if we have tool calls to execute
        if (!result.toolCalls || result.toolCalls.length === 0) {
          // No tool calls - we're done. End debug round.
          if (messageId) {
            debugStore.endDebugRound(messageId, model, iterations);
          }
          break;
        }

        // Resume streaming state so the UI shows between-round indicators
        // (completeStream set isStreaming=false, but we have more rounds coming)
        streamingStore.resumeStreaming(storeKey);

        // Capture tool calls for debug
        if (messageId) {
          debugStore.setRoundToolCalls(
            messageId,
            model,
            iterations,
            result.toolCalls.map((tc) => ({
              id: tc.id,
              name: tc.name,
              arguments: tc.arguments,
            }))
          );
        }

        // --- Tool Execution Timeline: Capture model reasoning from previous iteration ---
        // If this isn't the first iteration and the model output text before tool calls,
        // that text is the model's reasoning about why it's making the next tool call.
        // Associate this with the PREVIOUS round (shows the model's decision after seeing those results).
        if (
          iterations > 1 &&
          result.content &&
          result.content.trim() &&
          executionRounds.length > 0
        ) {
          const previousRound = executionRounds[executionRounds.length - 1];
          if (!previousRound.modelReasoning) {
            const reasoning = result.content.trim();
            previousRound.modelReasoning = reasoning;
            // Also update the store for real-time UI
            streamingStore.setRoundModelReasoning(storeKey, reasoning);
          }
        }

        // --- Tool Execution Timeline: Start new round ---
        const roundNumber = streamingStore.startExecutionRound(storeKey);

        // Create ToolExecution objects for each tool call
        // Note: inputArtifacts are initially empty; they'll be populated after execution
        // from the tool result artifacts that have role: 'input'
        const executions: ToolExecution[] = result.toolCalls.map((tc) => ({
          id: tc.id,
          toolName: tc.name,
          status: "running" as const,
          startTime: Date.now(),
          input: tc.arguments,
          inputArtifacts: [],
          outputArtifacts: [],
          round: roundNumber,
          statusMessage: getToolStatusLabel(tc.name, tc.name),
        }));

        // Add executions to store for real-time UI updates
        for (const exec of executions) {
          streamingStore.addToolExecution(storeKey, exec);
        }

        // Execute tool calls with status message callback for real-time UI updates
        const toolContext: ToolExecutorContext = {
          vectorStoreIds,
          token: token ?? undefined,
          signal: abortController.signal,
          onStatusMessage: (toolCallId, message) => {
            streamingStore.setToolExecutionStatusMessage(storeKey, toolCallId, message);
          },
          // Use configured sub-agent model, fall back to current streaming model
          defaultModel: subAgentModel || model,
        };

        const toolResults = await executeToolCalls(result.toolCalls, toolContext);

        // --- Tool Execution Timeline: Complete executions ---
        // Track completed executions for our local rounds array
        const completedExecutions: ToolExecution[] = [];

        for (const tc of result.toolCalls) {
          const toolResult = toolResults.get(tc.id);
          const execution = executions.find((e) => e.id === tc.id);

          if (!execution) continue;

          // Split artifacts by role - executors now set role: 'input' or 'output'
          const allArtifacts = toolResult?.artifacts ?? [];
          const inputArtifacts: Artifact[] = allArtifacts
            .filter((a) => a.role === "input")
            .map((a) => ({ ...a, toolCallId: tc.id }));
          const outputArtifacts: Artifact[] = allArtifacts
            .filter((a) => a.role !== "input") // Default to output if role not specified
            .map((a) => ({ ...a, role: "output" as const, toolCallId: tc.id }));

          // Complete the execution in the store
          streamingStore.completeToolExecution(
            storeKey,
            tc.id,
            inputArtifacts,
            outputArtifacts,
            toolResult?.error
          );

          // Track completed execution locally
          completedExecutions.push({
            ...execution,
            status: toolResult?.error ? "error" : "success",
            endTime: Date.now(),
            duration: Date.now() - execution.startTime,
            inputArtifacts,
            outputArtifacts,
            error: toolResult?.error,
          });

          // Also add citations and all artifacts to streaming store (for backward compatibility)
          if (toolResult?.citations && toolResult.citations.length > 0) {
            streamingStore.addCitations(storeKey, toolResult.citations);
          }
          if (allArtifacts.length > 0) {
            streamingStore.addArtifacts(storeKey, allArtifacts);
          }
        }

        // Build round for local tracking
        const round: ToolExecutionRound = {
          round: roundNumber,
          executions: completedExecutions,
          hasError: completedExecutions.some((e) => e.status === "error"),
          totalDuration: completedExecutions.reduce((sum, e) => sum + (e.duration ?? 0), 0),
        };
        executionRounds.push(round);

        // Attach tool execution to the current completed round
        if (allCompletedRounds.length > 0) {
          const lastIdx = allCompletedRounds.length - 1;
          allCompletedRounds[lastIdx] = { ...allCompletedRounds[lastIdx], toolExecution: round };
          streamingStore.setCompletedRoundToolExecution(storeKey, round);
        }

        // Build continuation input with tool results
        const toolResultItems = buildToolResultInputItems(result.toolCalls, toolResults);

        // Build the function_call items that the model outputted
        const functionCallItems = result.toolCalls.map((tc) => ({
          type: "function_call" as const,
          id: tc.id,
          call_id: tc.callId,
          name: tc.name,
          arguments: JSON.stringify(tc.arguments),
        }));

        // Extract assistant output message items from response output.
        // When the model emits text AND tool calls in the same turn, the text
        // must be included in the continuation so the next round has full context.
        const outputMessageItems = (result.responseOutput ?? []).filter((item) => {
          const obj = item as Record<string, unknown>;
          return obj.type === "message" && obj.role === "assistant";
        }) as Array<Record<string, unknown>>;

        // Capture debug data: tool results and continuation items
        if (messageId) {
          // Convert toolResults Map to array for debug store
          const toolResultsArray: Array<{
            callId: string;
            toolName: string;
            success: boolean;
            output?: string;
            error?: string;
          }> = [];
          for (const tc of result.toolCalls) {
            const toolResult = toolResults.get(tc.id);
            toolResultsArray.push({
              callId: tc.id,
              toolName: tc.name,
              success: toolResult?.success ?? false,
              output: toolResult?.output,
              error: toolResult?.error,
            });
          }
          debugStore.setRoundToolResults(messageId, model, iterations, toolResultsArray);

          // Capture continuation items (what gets sent to the next round)
          debugStore.setRoundContinuationItems(messageId, model, iterations, [
            ...outputMessageItems,
            ...functionCallItems,
            ...toolResultItems,
          ]);

          // End the debug round
          debugStore.endDebugRound(messageId, model, iterations);
        }

        // Continue conversation with function calls and their results
        currentInputItems = [
          ...currentInputItems,
          // The assistant's output message (text emitted before/alongside tool calls)
          ...outputMessageItems,
          // The assistant's function call(s)
          ...functionCallItems,
          // The tool results
          ...toolResultItems,
        ];

        // Clear tool calls from streaming store before next iteration
        streamingStore.clearToolCalls(storeKey);

        // Content for the next round will stream fresh (pushCompletedRound resets it)
      }

      // Complete debug capture successfully
      if (messageId) {
        debugStore.completeDebugCapture(messageId, model, true);
      }

      const flatContent = allCompletedRounds
        .map((r) => r.content)
        .filter(Boolean)
        .join("\n\n---\n\n");

      return {
        content: flatContent,
        hasOutputText: true,
        usage: accumulatedUsage,
        reasoningContent: lastReasoningContent,
        completedRounds: allCompletedRounds.length > 0 ? allCompletedRounds : undefined,
        toolExecutionRounds: executionRounds.length > 0 ? executionRounds : undefined,
      };
    },
    [
      clientSideToolExecution,
      streamResponse,
      vectorStoreIds,
      token,
      streamingStore,
      debugStore,
      maxToolIterations,
      captureRawSSEEvents,
      subAgentModel,
    ]
  );

  /**
   * Send message in "multiple" mode - all models respond in parallel
   * Uses selectedInstances to support multiple instances of the same model
   */
  const sendMultipleMode = useCallback(
    async (
      apiContent: string | unknown[],
      /** Optional message ID for debug capture - if provided, debug data will be keyed by this ID */
      debugMessageId?: string
    ): Promise<Array<ModeResult | null>> => {
      // Use instances if available, fall back to models for backwards compatibility
      const instances: ModelInstance[] =
        selectedInstances.length > 0
          ? selectedInstances
          : models.map((modelId) => ({ id: modelId, modelId }));

      // Build model map for initStreaming (instance ID -> model ID)
      const modelMap = new Map<string, string>();
      for (const instance of instances) {
        modelMap.set(instance.id, instance.modelId);
      }

      // Initialize streaming responses for each instance
      const instanceIds = instances.map((i) => i.id);
      streamingStore.initStreaming(instanceIds, modelMap);

      // Create abort controllers
      const controllers = instances.map(() => new AbortController());
      abortControllersRef.current = controllers;

      const filterFn = createFilterFn();

      // Stream responses from all instances in parallel
      // Use streamWithToolExecution to support client-side tool execution
      const responsePromises = instances.map((instance, index) => {
        const filteredMessages = filterFn(messages, instance.modelId);
        const inputItems = [
          ...filteredMessages.map(messageToApiInput),
          { role: "user", content: apiContent },
        ];
        return streamWithToolExecution(
          instance.modelId, // Use model ID for API call
          inputItems,
          controllers[index],
          settings,
          instance.id, // Use instance ID as streamId
          debugMessageId, // Pass debug message ID if provided
          instance.parameters, // Pass instance-specific parameters
          instance.label // Pass instance label for system prompt
        );
      });

      return Promise.all(responsePromises);
    },
    [
      models,
      selectedInstances,
      messages,
      settings,
      streamWithToolExecution,
      streamingStore,
      createFilterFn,
    ]
  );

  const sendMessage = useCallback(
    async (content: string, files: ChatFile[]) => {
      if (models.length === 0) return;

      // Snapshot the conversation we're sending into. If the user switches
      // conversations before the stream completes, the stored ref will diverge
      // and we drop the results below instead of writing them into the new
      // conversation's message list.
      const sendEpoch = conversationIdRef.current;

      // Add user message to conversation store (with the current historyMode).
      // The displayed message is just the user's text — any slash-invoked skill
      // is seeded into the API content below, not the visible bubble.
      addUserMessage(content, files.length > 0 ? files : undefined, historyMode);

      // Prepare message content for API (handles both plain text and multi-modal with files)
      let apiContent = buildApiContent(content, files.length > 0 ? files : undefined);

      // Seed a slash-invoked skill's SKILL.md directly into the request so it
      // loads deterministically (no reliance on the model calling the tool).
      const pendingSkillId = useChatUIStore.getState().pendingSkillId;
      if (pendingSkillId) {
        useChatUIStore.getState().clearPendingSkill();
        const seed = await loadSkillSeed(pendingSkillId);
        if (seed) {
          apiContent = seedSkillIntoApiContent(apiContent, seed.name, seed.text);
        }
      }

      // Generate a debug message ID for capturing request/response data
      // This will be used to key the debug info for this message exchange
      const debugMessageId = `msg_${Date.now()}_${Math.random().toString(36).slice(2, 9)}`;

      // Execute based on conversation mode
      let results: Array<ModeResult | null>;
      const ctx = createModeContext();

      if (conversationMode === "chained" && models.length > 1) {
        results = await sendChainedMode(apiContent, ctx);
      } else if (conversationMode === "routed" && models.length > 1) {
        results = await sendRoutedMode(apiContent, ctx);
      } else if (conversationMode === "synthesized" && models.length > 1) {
        results = await sendSynthesizedMode(apiContent, ctx, sendMultipleMode);
      } else if (conversationMode === "refined" && models.length > 1) {
        results = await sendRefinedMode(apiContent, ctx, sendMultipleMode);
      } else if (conversationMode === "critiqued" && models.length > 1) {
        results = await sendCritiquedMode(apiContent, ctx, sendMultipleMode);
      } else if (conversationMode === "elected" && models.length >= 3) {
        results = await sendElectedMode(apiContent, ctx, sendMultipleMode);
      } else if (conversationMode === "tournament" && models.length >= 4) {
        results = await sendTournamentMode(apiContent, ctx, sendMultipleMode);
      } else if (conversationMode === "consensus" && models.length >= 2) {
        results = await sendConsensusMode(apiContent, ctx, sendMultipleMode);
      } else if (conversationMode === "debated" && models.length >= 2) {
        results = await sendDebatedMode(apiContent, ctx, sendMultipleMode);
      } else if (conversationMode === "council" && models.length >= 2) {
        results = await sendCouncilMode(apiContent, ctx, sendMultipleMode);
      } else if (conversationMode === "hierarchical" && models.length >= 2) {
        results = await sendHierarchicalMode(apiContent, ctx, sendMultipleMode);
      } else if (conversationMode === "scattershot" && models.length >= 1) {
        results = await sendScattershotMode(apiContent, ctx, sendMultipleMode);
      } else if (conversationMode === "explainer" && models.length >= 1) {
        results = await sendExplainerMode(apiContent, ctx, sendMultipleMode);
      } else if (conversationMode === "confidence-weighted" && models.length >= 2) {
        results = await sendConfidenceWeightedMode(apiContent, ctx, sendMultipleMode);
      } else {
        // Default to multiple mode (parallel) - pass debug message ID
        results = await sendMultipleMode(apiContent, debugMessageId);
      }

      // Commit responses to conversation store (including errors)
      const allResponses: Array<{
        model: string;
        /** Instance ID for multi-instance support */
        instanceId?: string;
        content: string;
        usage?: MessageUsage;
        modeMetadata?: MessageModeMetadata;
        error?: string;
        citations?: Citation[];
        artifacts?: Artifact[];
        toolExecutionRounds?: ToolExecutionRound[];
        completedRounds?: CompletedRound[];
        /** Debug message ID for looking up debug info */
        debugMessageId?: string;
      }> = [];

      // Build instances for lookup (same logic as sendMultipleMode)
      const instances: ModelInstance[] =
        selectedInstances.length > 0
          ? selectedInstances
          : models.map((modelId) => ({ id: modelId, modelId }));

      // Get current streams to check for errors and citations (before clearing)
      const currentStreams = useStreamingStore.getState().streams;

      // Scattershot mode returns results per variation (not per model)
      if (conversationMode === "scattershot") {
        for (const result of results) {
          if (result !== null) {
            // Use variation label as the "model" name for display, falling back to variation ID
            const displayName = result.variationLabel || result.variationId || models[0];
            // Get citations and artifacts from stream (using the variation id/label or model)
            const stream = currentStreams.get(displayName) || currentStreams.get(models[0]);
            allResponses.push({
              model: displayName,
              content: result.content,
              usage: result.usage,
              modeMetadata: result.modeMetadata,
              ...commitFieldsFromStream(stream),
            });
          }
        }
      } else if (conversationMode === "explainer") {
        // Explainer mode returns results per audience level (not per model)
        for (const result of results) {
          if (result !== null) {
            // Use level label as the "model" name for display
            const displayName = result.levelLabel || "Explanation";
            const stream = currentStreams.get(displayName);
            allResponses.push({
              model: displayName,
              content: result.content,
              usage: result.usage,
              modeMetadata: result.modeMetadata,
              ...commitFieldsFromStream(stream),
            });
          }
        }
      } else {
        // Standard handling: results map 1:1 to instances
        for (let index = 0; index < instances.length; index++) {
          const result = results[index];
          const instance = instances[index];
          // Use instance ID for stream lookup (streams are keyed by instance ID)
          const stream = currentStreams.get(instance.id);
          // Only include debugMessageId for multiple mode (default)
          const msgDebugId = conversationMode === "multiple" ? debugMessageId : undefined;
          if (result !== null) {
            allResponses.push({
              model: instance.modelId,
              instanceId: instance.id,
              content: result.content,
              usage: result.usage,
              modeMetadata: result.modeMetadata,
              debugMessageId: msgDebugId,
              ...commitFieldsFromStream(stream),
            });
          } else if (stream?.error) {
            allResponses.push({
              model: instance.modelId,
              instanceId: instance.id,
              content: "",
              error: stream.error,
              debugMessageId: msgDebugId,
              ...commitFieldsFromStream(stream),
            });
          }
        }
      }

      // Drop results if the user switched conversations during the stream —
      // committing them now would attach them to the wrong conversation.
      // sendEpoch === undefined means there was no conversation when we kicked
      // off (the send itself just created one), so we always commit those.
      if (
        (sendEpoch === undefined || sendEpoch === conversationIdRef.current) &&
        allResponses.length > 0
      ) {
        addAssistantMessages(allResponses);
      }

      // Clear streaming state
      streamingStore.clearStreams();
      abortControllersRef.current = [];
    },
    [
      models,
      selectedInstances,
      conversationMode,
      historyMode,
      sendMultipleMode,
      createModeContext,
      streamingStore,
      addUserMessage,
      addAssistantMessages,
    ]
  );

  const clearMessages = useCallback(() => {
    stopStreaming();
    setMessages([]);
    streamingStore.clearStreams();
  }, [stopStreaming, setMessages, streamingStore]);

  const { replaceAssistantMessage, updateMessage, deleteMessagesAfter } = useConversationStore();

  const regenerateResponse = useCallback(
    async (userMessageId: string, model: string) => {
      // Find the user message
      const userMessageIndex = messages.findIndex((m) => m.id === userMessageId);
      if (userMessageIndex === -1) return;

      const userMessage = messages[userMessageIndex];
      if (userMessage.role !== "user") return;

      const sendEpoch = conversationIdRef.current;

      // Get all messages up to and including the user message, filtered by the history mode
      // that was stored on that user message (use current historyMode as fallback for old messages)
      const messageHistoryMode = userMessage.historyMode ?? historyMode;
      const messagesUpToUser = messages.slice(0, userMessageIndex + 1);
      const filteredMessages = filterMessagesForModel(messagesUpToUser, model, messageHistoryMode);

      // Prepare input items for Responses API (includes files from previous messages)
      const inputItems = filteredMessages.map(messageToApiInput);

      // Initialize streaming for regeneration (single model)
      streamingStore.initStreaming([model]);

      // Create abort controller
      const controller = new AbortController();
      abortControllersRef.current = [controller];

      const debugMessageId = `msg_${Date.now()}_${Math.random().toString(36).slice(2, 9)}`;

      // Stream the response (with tool execution support)
      const result = await streamWithToolExecution(
        model,
        inputItems,
        controller,
        settings,
        model,
        debugMessageId
      );

      if (result !== null && sendEpoch === conversationIdRef.current) {
        const stream = useStreamingStore.getState().streams.get(model);
        replaceAssistantMessage(userMessageId, model, {
          content: result.content,
          usage: result.usage,
          debugMessageId,
          ...commitFieldsFromStream(stream),
        });
      }

      // Clear streaming state
      streamingStore.clearStreams();
      abortControllersRef.current = [];
    },
    [
      messages,
      settings,
      historyMode,
      streamWithToolExecution,
      streamingStore,
      replaceAssistantMessage,
    ]
  );

  /**
   * Resume a gateway MCP tool call that paused for human approval.
   *
   * Re-streams the assistant turn with an `mcp_approval_response` input item
   * appended; Hadrian claims the parked approval by `approvalRequestId`,
   * runs (or refuses) the call, and continues the model loop. The continued
   * output is appended to the same assistant message. Single-model turns only
   * — gateway MCP approvals aren't surfaced in multi-model modes.
   */
  const respondToMcpApproval = useCallback(
    async (assistantMessageId: string, approvalRequestId: string, approve: boolean) => {
      // Don't start an approval-resume turn while another turn is
      // streaming — it would clobber the active AbortController and race
      // the in-flight stream. The button is also disabled in this state
      // (see ChatMessageList), this is the belt-and-suspenders guard.
      if (useStreamingStore.getState().isStreaming) return;
      // Read messages live from the store rather than the closed-over
      // snapshot so the resumed request is built from current state, not
      // whatever was rendered when the handler was created.
      const liveMessages = useConversationStore.getState().messages;
      const idx = liveMessages.findIndex((m) => m.id === assistantMessageId);
      if (idx === -1) return;
      const assistantMessage = liveMessages[idx];
      if (assistantMessage.role !== "assistant") return;

      const model = assistantMessage.model ?? models[0];
      if (!model) return;
      const sendEpoch = conversationIdRef.current;

      // Optimistically record the decision so the prompt flips to resolved.
      const resolvedApprovals = (assistantMessage.pendingMcpApprovals ?? []).map((a) =>
        a.approvalRequestId === approvalRequestId
          ? { ...a, resolved: approve ? ("approved" as const) : ("denied" as const) }
          : a
      );
      updateMessage(assistantMessageId, { pendingMcpApprovals: resolvedApprovals });

      // Build history through this assistant turn, then echo the approval back.
      const messagesThroughAssistant = liveMessages.slice(0, idx + 1);
      const filtered = filterMessagesForModel(messagesThroughAssistant, model, historyMode);
      const inputItems = [
        ...filtered.map(messageToApiInput),
        {
          type: "mcp_approval_response",
          approval_request_id: approvalRequestId,
          approve,
        },
      ];

      streamingStore.initStreaming([model]);
      const controller = new AbortController();
      abortControllersRef.current = [controller];
      const debugMessageId = `msg_${Date.now()}_${Math.random().toString(36).slice(2, 9)}`;

      const result = await streamWithToolExecution(
        model,
        inputItems,
        controller,
        settings,
        model,
        debugMessageId
      );

      if (result !== null && sendEpoch === conversationIdRef.current) {
        const stream = useStreamingStore.getState().streams.get(model);
        const committed = commitFieldsFromStream(stream);
        const continued = result.content
          ? `${assistantMessage.content ? `${assistantMessage.content}\n\n` : ""}${result.content}`
          : assistantMessage.content;
        updateMessage(assistantMessageId, {
          content: continued,
          usage: result.usage ?? assistantMessage.usage,
          ...committed,
          // Preserve the resolved decision unless the continuation parked a new one.
          pendingMcpApprovals: committed.pendingMcpApprovals ?? resolvedApprovals,
        });
      }

      streamingStore.clearStreams();
      abortControllersRef.current = [];
    },
    // `messages` is intentionally not a dep: the handler reads live store
    // state (useConversationStore.getState()) so it can't act on a stale
    // snapshot, and keeping the callback identity stable lets the
    // disabled-while-streaming guard in ChatMessageList stay reliable.
    [models, settings, historyMode, streamWithToolExecution, streamingStore, updateMessage]
  );

  /**
   * Edit a message and re-run the conversation from that point.
   * For user messages: updates content, deletes all subsequent messages, and streams new responses.
   * For assistant messages: updates content only (preserves sibling model responses).
   */
  const editAndRerun = useCallback(
    async (messageId: string, newContent: string) => {
      // Find the message
      const messageIndex = messages.findIndex((m) => m.id === messageId);
      if (messageIndex === -1) return;

      const message = messages[messageIndex];

      // Update the message content
      updateMessage(messageId, { content: newContent });

      // If it's a user message, delete subsequent messages and re-run to get new responses
      // For assistant messages, we only update the content (no deletion of sibling responses)
      if (message.role === "user") {
        const sendEpoch = conversationIdRef.current;

        // Delete all messages after the edited user message
        deleteMessagesAfter(messageId);

        // Use instances if available, fall back to models for backwards compatibility
        const instances: ModelInstance[] =
          selectedInstances.length > 0
            ? selectedInstances
            : models.map((modelId) => ({ id: modelId, modelId }));

        // Build model map for initStreaming (instance ID -> model ID)
        const modelMap = new Map<string, string>();
        for (const instance of instances) {
          modelMap.set(instance.id, instance.modelId);
        }

        // Initialize streaming responses for each instance
        const instanceIds = instances.map((i) => i.id);
        streamingStore.initStreaming(instanceIds, modelMap);

        // Create abort controllers
        const controllers = instances.map(() => new AbortController());
        abortControllersRef.current = controllers;

        const filterFn = createFilterFn();
        const debugMessageId = `msg_${Date.now()}_${Math.random().toString(36).slice(2, 9)}`;

        // Get messages up to and including the edited message (use updated content)
        // We need to read the latest messages from the store after our updates
        const currentMessages = useConversationStore.getState().messages;

        // Stream responses from all instances in parallel
        const responsePromises = instances.map((instance, index) => {
          const filteredMessages = filterFn(currentMessages, instance.modelId);
          const inputItems = filteredMessages.map(messageToApiInput);
          return streamWithToolExecution(
            instance.modelId,
            inputItems,
            controllers[index],
            settings,
            instance.id,
            debugMessageId,
            instance.parameters,
            instance.label
          );
        });

        const results = await Promise.all(responsePromises);

        // Get current streams to check for errors and citations (before clearing)
        const currentStreams = useStreamingStore.getState().streams;

        // Commit responses to conversation store
        const allResponses: Array<{
          model: string;
          instanceId?: string;
          content: string;
          usage?: MessageUsage;
          error?: string;
          citations?: Citation[];
          artifacts?: Artifact[];
          toolExecutionRounds?: ToolExecutionRound[];
          completedRounds?: CompletedRound[];
          debugMessageId?: string;
        }> = [];

        for (let index = 0; index < instances.length; index++) {
          const result = results[index];
          const instance = instances[index];
          const stream = currentStreams.get(instance.id);
          if (result !== null) {
            allResponses.push({
              model: instance.modelId,
              instanceId: instance.id,
              content: result.content,
              usage: result.usage,
              debugMessageId,
              ...commitFieldsFromStream(stream),
            });
          } else if (stream?.error) {
            allResponses.push({
              model: instance.modelId,
              instanceId: instance.id,
              content: "",
              error: stream.error,
              debugMessageId,
              ...commitFieldsFromStream(stream),
            });
          }
        }

        if (sendEpoch === conversationIdRef.current && allResponses.length > 0) {
          addAssistantMessages(allResponses);
        }

        // Clear streaming state
        streamingStore.clearStreams();
        abortControllersRef.current = [];
      }
      // For assistant messages, we just update content (no deletion, no re-run)
    },
    [
      messages,
      models,
      selectedInstances,
      settings,
      updateMessage,
      deleteMessagesAfter,
      streamWithToolExecution,
      streamingStore,
      createFilterFn,
      addAssistantMessages,
    ]
  );

  return {
    messages,
    modelResponses,
    isStreaming,
    sendMessage,
    stopStreaming,
    clearMessages,
    setMessages,
    regenerateResponse,
    editAndRerun,
    respondToMcpApproval,
  };
}
