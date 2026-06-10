import type {
  MessageModeMetadata,
  MessageUsage,
  RefinementRoundData,
  CritiqueRoundData,
  ModeConfig,
  ModelInstance,
  ModelParameters,
} from "@/components/chat-types";
import type { StreamingStore } from "@/stores/streamingStore";
import type { ChatMessage, ModelSettings } from "../types";

/** Result from a mode-specific send function */
export interface ModeResult {
  content: string;
  usage?: MessageUsage;
  modeMetadata?: MessageModeMetadata;
  /** For scattershot mode: the variation ID to use as model identifier */
  variationId?: string;
  /** For scattershot mode: human-readable variation label */
  variationLabel?: string;
  /** For explainer mode: human-readable audience level label */
  levelLabel?: string;
}

/** Common dependencies for mode handlers */
export interface ModeContext {
  models: string[];
  /**
   * Model instances with their configurations.
   * When provided, modes should prefer using instances over models.
   * Each instance has an id (unique), modelId (the actual model), label, and parameters.
   * If not provided, modes can derive instances from models using getContextInstances().
   */
  instances?: ModelInstance[];
  messages: ChatMessage[];
  settings?: ModelSettings;
  modeConfig?: ModeConfig;
  token: string;
  streamingStore: StreamingStore;
  abortControllersRef: React.MutableRefObject<AbortController[]>;
  streamResponse: StreamResponseFn;
  filterMessagesForModel: FilterMessagesFn;
  /** User message content (added by runner for spec access) */
  apiContent?: string | unknown[];
  /** Attached vector store IDs for file_search tool (RAG) */
  vectorStoreIds?: string[];
}

/**
 * Get instances from context, deriving from models if not provided.
 * This provides backwards compatibility for modes that haven't been updated yet.
 */
export function getContextInstances(ctx: ModeContext): ModelInstance[] {
  if (ctx.instances && ctx.instances.length > 0) {
    return ctx.instances;
  }
  // Derive instances from models (backwards compatibility)
  return ctx.models.map((modelId) => ({
    id: modelId,
    modelId,
  }));
}

/**
 * Find a special instance (synthesizer, router, coordinator, etc.) by:
 * 1. Instance ID from config (if provided)
 * 2. Model ID from config (find first instance with matching modelId)
 * 3. Fall back to first instance
 *
 * Returns the instance ID to use.
 */
export function findSpecialInstanceId(
  instances: ModelInstance[],
  instanceIdConfig: string | undefined,
  modelIdConfig: string | undefined
): string | undefined {
  if (instanceIdConfig) {
    return instanceIdConfig;
  }
  if (modelIdConfig) {
    const instanceByModel = instances.find((inst) => inst.modelId === modelIdConfig);
    if (instanceByModel) {
      return instanceByModel.id;
    }
  }
  return instances[0]?.id;
}

/** Function signature for streaming a response from a model */
export type StreamResponseFn = (
  model: string,
  inputItems: Array<{ role: string; content: string | unknown[] }>,
  abortController: AbortController,
  modelSettings?: ModelSettings,
  /** Optional stream ID for tracking in streamingStore (defaults to model) */
  streamId?: string,
  /** Whether to track tool calls (used internally by streamWithToolExecution) */
  trackToolCalls?: boolean,
  /** Optional callback for capturing SSE events (for debugging) */
  onSSEEvent?: (event: { type: string; timestamp: number; data: unknown }) => void,
  /** Optional instance-specific parameters (overrides perModelSettings lookup) */
  instanceParams?: ModelParameters,
  /** Optional instance label for system prompt identity */
  instanceLabel?: string
) => Promise<{ content: string; usage?: MessageUsage } | null>;

/** Function signature for filtering messages based on history mode */
export type FilterMessagesFn = (messages: ChatMessage[], targetModel: string) => ChatMessage[];

/** Usage data from provider (Responses API format) */
export interface ResponsesUsage {
  input_tokens: number;
  output_tokens: number;
  total_tokens: number;
  cost?: number;
  input_tokens_details?: {
    cached_tokens?: number;
  };
  output_tokens_details?: {
    reasoning_tokens?: number;
  };
}

/** Responses API streaming event types */
export interface ResponsesStreamEvent {
  type: string;
  delta?: string;
  text?: string;
  /**
   * Cumulative loop usage for Hadrian's `response.usage.updated` events —
   * top-level, unlike terminal events' `response.usage`.
   */
  usage?: ResponsesUsage;
  /** Item ID for tool call events (e.g., image_generation_call, file_search_call) */
  item_id?: string;
  /** Output index for tool call events */
  output_index?: number;
  /** Partial base64 image data for image_generation_call.partial_image events */
  partial_image_b64?: string;
  /** Output item for response.output_item.done events (e.g., file_search_call, image_generation_call) */
  item?: {
    type: string;
    /** Item ID */
    id?: string;
    /** Result data (e.g., base64 data URL for image_generation_call) */
    result?: string;
    /** Item status */
    status?: string;
    /** For file_search_call items */
    results?: Array<{
      file_id: string;
      filename: string;
      score: number;
      content?: Array<{ type: string; text: string }>;
    }>;
    /** Server label for gateway MCP items (mcp_call, mcp_list_tools, mcp_approval_request) */
    server_label?: string;
    /** Tool name for mcp_call / mcp_approval_request items */
    name?: string;
    /** JSON-encoded arguments for mcp_call / mcp_approval_request items */
    arguments?: string;
    /** Tool output for completed mcp_call items */
    output?: string;
    /** Error message for failed mcp_call items */
    error?: string | null;
    /** Approval request id to echo back in an mcp_approval_response */
    approval_request_id?: string;
    /** Tools discovered by an mcp_list_tools item */
    tools?: Array<{ name: string }>;
    /** Shell-call action object (`shell_call` items): the commands to run. */
    action?: { commands?: string[] };
    /** Call id that pairs a shell_call with its shell_call_output. */
    call_id?: string;
  };
  response?: {
    id: string;
    model: string;
    status: string;
    /** Container the shell tool used, injected on terminal events. */
    container_id?: string;
    output_text?: string;
    output?: Array<{
      type: string;
      /** Item ID */
      id?: string;
      /** Result data (e.g., base64 data URL for image_generation_call) */
      result?: string;
      /** Item status */
      status?: string;
      /** Reasoning text content items */
      content?: Array<{
        type: string;
        text?: string;
      }>;
      /** Reasoning summary items */
      summary?: Array<{
        type: string;
        text?: string;
      }>;
    }>;
    usage?: ResponsesUsage;
  };
}

// Re-export types that are commonly used
export type { MessageModeMetadata, MessageUsage, RefinementRoundData, CritiqueRoundData };
