import { create } from "zustand";
import { useShallow } from "zustand/react/shallow";

import type {
  MessageUsage,
  ResponseFeedbackData,
  Citation,
  Artifact,
  CompletedRound,
  ToolExecution,
  ToolExecutionRound,
  McpApprovalRequest,
} from "@/components/chat-types";
import type { ToolCallState } from "@/pages/chat/utils/toolCallParser";

// Re-export ToolCallState for convenience
export type { ToolCallState } from "@/pages/chat/utils/toolCallParser";

/**
 * Streaming Store - Ephemeral State for Real-Time Token Streaming
 *
 * ## Architecture Overview
 *
 * This store manages **ephemeral** streaming state that is separate from the committed
 * conversation state in `conversationStore`. This separation is critical for performance:
 *
 * - **High-frequency updates**: Token streaming can deliver 50-100+ tokens/second.
 *   Each token triggers a state update, but only components subscribed to the specific
 *   model's stream will re-render.
 *
 * - **Isolation**: The `conversationStore` (persistent messages) never sees individual
 *   token updates. It only receives the final content when streaming completes.
 *
 * ## Re-render Behavior
 *
 * When a token arrives for model "claude-opus":
 * ```
 * appendContent("claude-opus", "Hello")
 *     │
 *     ▼
 * Only components using useStreamContent("claude-opus") re-render
 *     ├── StreamingMessage for claude-opus  ✅ RE-RENDERS
 *     ├── StreamingMessage for gpt-4        ❌ NO RE-RENDER
 *     ├── MemoizedMessage components        ❌ NO RE-RENDER
 *     └── ChatMessageList                   ❌ NO RE-RENDER
 * ```
 *
 * ## Usage Pattern
 *
 * 1. `initStreaming(models)` - Called when user sends message, creates empty streams
 * 2. `appendContent(model, delta)` - Called for each SSE token event
 * 3. `setContent(model, content)` - Called on `response.output_text.done` for final correction
 * 4. `completeStream(model, usage)` - Called on `response.completed` with usage data
 * 5. Content committed to `conversationStore.addAssistantMessages()`
 * 6. `clearStreams()` - Reset for next message
 *
 * ## Key Files
 * - `useChat.ts` - Orchestrates the streaming flow
 * - `ChatMessageList.tsx` - Renders streaming responses outside virtualization
 * - `MultiModelResponse.tsx` - Renders individual model response cards
 */

/** State for a single streaming model response */
export interface StreamingResponse {
  /** Model ID (e.g., "openai/gpt-4") used for API calls */
  model: string;
  /**
   * Instance ID uniquely identifying this stream.
   * For multi-instance scenarios (same model, different settings),
   * this distinguishes between instances (e.g., "gpt-4-creative" vs "gpt-4-precise").
   * Falls back to model ID if not set.
   */
  instanceId?: string;
  content: string;
  /** Reasoning content for the current round (extended thinking) */
  reasoningContent: string;
  /** Completed rounds bundling reasoning, content, and tool execution (multi-round tool execution) */
  completedRounds: CompletedRound[];
  isStreaming: boolean;
  error?: string;
  usage?: MessageUsage;

  // Timing stats (for "stats for nerds" feature)
  /** Timestamp when streaming started (ms since epoch) */
  startTime?: number;
  /** Timestamp when first content token was received (ms since epoch) */
  firstTokenTime?: number;
  feedback?: ResponseFeedbackData;
  /**
   * Tool calls requested by the model during this response.
   * Used for client-side tool execution (e.g., file_search).
   * Map key is the tool call ID (e.g., "fc_xxx" or "toolu_xxx").
   */
  toolCalls?: Map<string, ToolCallState>;
  /**
   * Citations from file_search or web_search tool results.
   * Populated from tool execution results (client-side) or SSE events (server-side).
   */
  citations?: Citation[];
  /**
   * Artifacts produced by tool execution (charts, tables, images, code output).
   * These are rich output objects displayed in the UI but not sent to the model.
   */
  artifacts?: Artifact[];
  /**
   * Gateway MCP tool calls that paused for human approval during this
   * response. Rendered as approve/deny prompts; resolved entries stay so the
   * transcript records the decision.
   */
  mcpApprovals?: McpApprovalRequest[];
  /**
   * Tool execution timeline tracking rounds of tool calls.
   * Each round contains one or more tool executions and optional model reasoning.
   * Used for progressive disclosure UI showing execution history.
   */
  toolExecutionRounds?: ToolExecutionRound[];
}

/** Routing state for routed mode */
export interface RoutingState {
  /** Current phase: routing (selecting model) or selected (model chosen) */
  phase: "routing" | "selected";
  /** The model acting as router */
  routerModel: string;
  /** The selected target model (null during routing phase) */
  selectedModel: string | null;
  /** Optional reasoning from the router */
  reasoning?: string;
  /** Whether the selected model is a fallback (router failed to select or returned invalid model) */
  isFallback?: boolean;
}

/** Source response from a model (used in synthesized mode) */
export interface SourceResponse {
  model: string;
  content: string;
  usage?: MessageUsage;
}

/** Synthesis state for synthesized mode */
export interface SynthesisState {
  /** Current phase: gathering (parallel responses) or synthesizing (combining) or done */
  phase: "gathering" | "synthesizing" | "done";
  /** The model performing synthesis */
  synthesizerModel: string;
  /** Models that have completed their responses */
  completedModels: string[];
  /** Total number of models responding */
  totalModels: number;
  /** Collected source responses (populated as models complete) */
  sourceResponses: SourceResponse[];
}

/** Single refinement round result */
export interface RefinementRound {
  /** The model that performed this refinement */
  model: string;
  /** The content after this refinement */
  content: string;
  /** Token usage for this round */
  usage?: MessageUsage;
}

/** Refinement state for refined mode */
export interface RefinementState {
  /** Current phase: initial (first response) or refining (subsequent rounds) or done */
  phase: "initial" | "refining" | "done";
  /** Current round number (0-indexed) */
  currentRound: number;
  /** Total number of rounds planned */
  totalRounds: number;
  /** The model currently generating/refining */
  currentModel: string;
  /** History of all refinement rounds (including initial) */
  rounds: RefinementRound[];
}

/** Single critique from a model */
export interface CritiqueData {
  /** The model that provided this critique */
  model: string;
  /** The critique content */
  content: string;
  /** Token usage for this critique */
  usage?: MessageUsage;
}

/** Critique state for critiqued mode */
export interface CritiqueState {
  /** Current phase: initial (primary response), critiquing (gathering critiques), revising (final revision), or done */
  phase: "initial" | "critiquing" | "revising" | "done";
  /** The model providing the initial response and revision */
  primaryModel: string;
  /** The initial response content */
  initialResponse?: string;
  /** Usage from the initial response */
  initialUsage?: MessageUsage;
  /** Models that will provide critiques */
  critiqueModels: string[];
  /** Collected critiques (populated as critic models complete) */
  critiques: CritiqueData[];
  /** Number of critiques completed */
  completedCritiques: number;
}

/** Single vote from a model in elected mode */
export interface VoteData {
  /** The model that cast this vote */
  voter: string;
  /** The model being voted for */
  votedFor: string;
  /** Optional reasoning for the vote */
  reasoning?: string;
  /** Token usage for this vote */
  usage?: MessageUsage;
}

/** Candidate response in elected mode */
export interface CandidateResponse {
  /** The model that generated this response */
  model: string;
  /** The response content */
  content: string;
  /** Token usage for this response */
  usage?: MessageUsage;
}

/** Election state for elected mode */
export interface ElectionState {
  /** Current phase: responding (generating candidates), voting (casting votes), or done */
  phase: "responding" | "voting" | "done";
  /** All candidate responses */
  candidates: CandidateResponse[];
  /** Number of models that have completed their response */
  completedResponses: number;
  /** Total number of models responding */
  totalModels: number;
  /** Votes cast (populated during voting phase) */
  votes: VoteData[];
  /** Number of votes completed */
  completedVotes: number;
  /** The winning model (set in done phase) */
  winner?: string;
  /** Vote counts per model */
  voteCounts?: Record<string, number>;
}

/** Single tournament match (streaming state) */
export interface TournamentMatch {
  /** Match ID (round-match format) */
  id: string;
  /** Round number (0-indexed) */
  round: number;
  /** Competitor 1 model ID */
  competitor1: string;
  /** Competitor 2 model ID */
  competitor2: string;
  /** Status of this match */
  status: "pending" | "generating" | "judging" | "complete";
  /** Competitor 1's response (populated during/after generating) */
  response1?: string;
  /** Competitor 2's response */
  response2?: string;
  /** Usage from competitor 1 */
  usage1?: MessageUsage;
  /** Usage from competitor 2 */
  usage2?: MessageUsage;
  /** The winner (set when complete) */
  winner?: string;
  /** Judge model */
  judge?: string;
  /** Judge's reasoning */
  reasoning?: string;
  /** Usage from judge */
  judgeUsage?: MessageUsage;
}

/** Tournament state for tournament mode */
export interface TournamentState {
  /** Current phase: generating (initial responses), competing (bracket matches), or done */
  phase: "generating" | "competing" | "done";
  /** The bracket structure: models at each round (winners advance) */
  bracket: string[][];
  /** Current round number (0-indexed) */
  currentRound: number;
  /** Total number of rounds */
  totalRounds: number;
  /** All matches across all rounds */
  matches: TournamentMatch[];
  /** Current match being processed (null if between matches) */
  currentMatch?: string;
  /** Initial responses from all models (for first round) */
  initialResponses: CandidateResponse[];
  /** Models eliminated at each round */
  eliminatedPerRound: string[][];
  /** The final winner (set in done phase) */
  winner?: string;
}

/** Single consensus round result */
export interface ConsensusRound {
  /** Round number (0 = initial) */
  round: number;
  /** Responses from all models in this round */
  responses: CandidateResponse[];
  /** Whether consensus was reached in this round */
  consensusReached: boolean;
  /** Consensus score for this round (0-1) */
  consensusScore?: number;
}

/** Single debate turn/argument */
export interface DebateTurn {
  /** The model that made this argument */
  model: string;
  /** The position being argued (e.g., "pro", "con", or a custom perspective) */
  position: string;
  /** The argument content */
  content: string;
  /** Which round this turn belongs to (0-indexed) */
  round: number;
  /** Token usage for this turn */
  usage?: MessageUsage;
}

/** Single council statement */
export interface CouncilStatement {
  /** The model that made this statement */
  model: string;
  /** The role/perspective this model represents */
  role: string;
  /** The statement content */
  content: string;
  /** Which round this statement belongs to (0 = opening) */
  round: number;
  /** Token usage for this statement */
  usage?: MessageUsage;
}

/** Consensus state for consensus mode */
export interface ConsensusState {
  /** Current phase: responding (initial), revising (subsequent rounds), or done */
  phase: "responding" | "revising" | "done";
  /** Current round number (0-indexed) */
  currentRound: number;
  /** Maximum rounds allowed */
  maxRounds: number;
  /** Threshold for consensus (0-1) */
  threshold: number;
  /** All rounds completed so far */
  rounds: ConsensusRound[];
  /** Final consensus score (set when done) */
  finalScore?: number;
  /** Responses collected for the current round */
  currentRoundResponses: CandidateResponse[];
}

/** Debate state for debated mode */
export interface DebateState {
  /** Current phase: opening (initial positions), debating (back-and-forth), summarizing, or done */
  phase: "opening" | "debating" | "summarizing" | "done";
  /** Current debate round (0 = opening statements) */
  currentRound: number;
  /** Total number of debate rounds (excluding opening) */
  totalRounds: number;
  /** Model positions: map from model ID to assigned position */
  positions: Record<string, string>;
  /** All debate turns/arguments */
  turns: DebateTurn[];
  /** Current round's turns being collected */
  currentRoundTurns: DebateTurn[];
  /** The model that summarizes (optional, defaults to first) */
  summarizerModel?: string;
  /** Final summary content */
  summary?: string;
  /** Summary usage */
  summaryUsage?: MessageUsage;
}

/** Council state for council mode */
export interface CouncilState {
  /** Current phase: assigning (auto-assigning roles), opening (initial perspectives), discussing (rounds of discussion), synthesizing, or done */
  phase: "assigning" | "opening" | "discussing" | "synthesizing" | "done";
  /** Current discussion round (0 = opening statements) */
  currentRound: number;
  /** Total number of discussion rounds (excluding opening) */
  totalRounds: number;
  /** Model roles: map from model ID to assigned role/perspective */
  roles: Record<string, string>;
  /** All council statements */
  statements: CouncilStatement[];
  /** Current round's statements being collected */
  currentRoundStatements: CouncilStatement[];
  /** The model that synthesizes (optional, defaults to first) */
  synthesizerModel?: string;
  /** Final synthesis content */
  synthesis?: string;
  /** Synthesis usage */
  synthesisUsage?: MessageUsage;
}

/** Hierarchical subtask definition */
export interface HierarchicalSubtask {
  /** Subtask identifier */
  id: string;
  /** Description of the subtask */
  description: string;
  /** Model assigned to this subtask */
  assignedModel: string;
  /** Instance ID assigned to this subtask (falls back to assignedModel if not set) */
  assignedInstanceId?: string;
  /** Current status */
  status: "pending" | "in_progress" | "complete" | "failed";
  /** Result content (set when complete) */
  result?: string;
}

/** Worker result in hierarchical mode */
export interface HierarchicalWorkerResult {
  /** Subtask ID this result is for */
  subtaskId: string;
  /** Model that completed this subtask */
  model: string;
  /** Description of the subtask */
  description: string;
  /** Result content */
  content: string;
  /** Token usage */
  usage?: MessageUsage;
}

/** Hierarchical state for hierarchical mode */
export interface HierarchicalState {
  /** Current phase: decomposing (coordinator breaking down task), executing (workers working), synthesizing (coordinator combining), or done */
  phase: "decomposing" | "executing" | "synthesizing" | "done";
  /** The coordinator model */
  coordinatorModel: string;
  /** All subtasks (populated after decomposition) */
  subtasks: HierarchicalSubtask[];
  /** Completed worker results */
  workerResults: HierarchicalWorkerResult[];
  /** Final synthesis content */
  synthesis?: string;
  /** Usage from decomposition step */
  decompositionUsage?: MessageUsage;
  /** Usage from synthesis step */
  synthesisUsage?: MessageUsage;
}

/** Scattershot variation parameters (imported from chat-types) */
export interface ScattershotParams {
  temperature?: number;
  maxTokens?: number;
  topP?: number;
  topK?: number;
  frequencyPenalty?: number;
  presencePenalty?: number;
}

/** Single scattershot variation result */
export interface ScattershotVariation {
  /** Unique identifier for this variation */
  id: string;
  /** Index in the variations array */
  index: number;
  /** Parameter overrides for this variation */
  params: ScattershotParams;
  /** Human-readable label for the variation */
  label: string;
  /** Current status */
  status: "pending" | "generating" | "complete" | "failed";
  /** Response content (set when complete) */
  content?: string;
  /** Token usage */
  usage?: MessageUsage;
}

/** Scattershot state for scattershot mode */
export interface ScattershotState {
  /** Current phase: generating (running variations) or done */
  phase: "generating" | "done";
  /** The model being used for all variations */
  targetModel: string;
  /** All variations */
  variations: ScattershotVariation[];
}

/** Single explanation at a specific audience level */
export interface ExplanationLevel {
  /** The audience level (e.g., "expert", "intermediate", "beginner") */
  level: string;
  /** The model that generated this explanation */
  model: string;
  /** The explanation content */
  content: string;
  /** Token usage */
  usage?: MessageUsage;
}

/** Explainer state for explainer mode */
export interface ExplainerState {
  /** Current phase: initial (first explanation), simplifying (progressive simplification), or done */
  phase: "initial" | "simplifying" | "done";
  /** The target audience levels in order (e.g., ["expert", "intermediate", "beginner"]) */
  audienceLevels: string[];
  /** Current level index being generated (0-indexed) */
  currentLevelIndex: number;
  /** All explanations generated so far */
  explanations: ExplanationLevel[];
  /** The model generating the current explanation */
  currentModel?: string;
}

/** Single response with confidence score */
export interface ConfidenceResponse {
  /** The model that generated this response */
  model: string;
  /** The response content */
  content: string;
  /** Self-assessed confidence score (0-1) */
  confidence: number;
  /** Token usage for this response */
  usage?: MessageUsage;
}

/** Confidence-weighted state for confidence-weighted mode */
export interface ConfidenceWeightedState {
  /** Current phase: responding (models generate with confidence), synthesizing (weighting), or done */
  phase: "responding" | "synthesizing" | "done";
  /** All responses with confidence scores */
  responses: ConfidenceResponse[];
  /** Number of responses completed */
  completedResponses: number;
  /** Total number of models responding */
  totalModels: number;
  /** The model performing synthesis */
  synthesizerModel: string;
  /** Final weighted synthesis content */
  synthesis?: string;
  /** Synthesis usage */
  synthesisUsage?: MessageUsage;
}

/**
 * ActiveModeState - Discriminated Union for All Mode States
 *
 * This union type consolidates all mode-specific state into a single field,
 * replacing the 13+ nullable fields in StreamingState. Benefits:
 *
 * 1. **Single source of truth** - Only one mode can be active at a time
 * 2. **Type safety** - TypeScript discriminated union enables exhaustive checking
 * 3. **Simpler selectors** - `useModeState()` returns typed union instead of checking 13 fields
 * 4. **Easier cleanup** - `clearStreams()` just sets `{ mode: null }`
 *
 * Each variant includes a `mode` discriminator matching the ConversationMode value.
 */
export type ActiveModeState =
  | { mode: null }
  | { mode: "chained"; position: [number, number] }
  | {
      mode: "routed";
      phase: "routing" | "selected";
      routerModel: string;
      routerInstanceId: string;
      selectedModel: string | null;
      selectedInstanceId: string | null;
      reasoning?: string;
      isFallback?: boolean;
    }
  | {
      mode: "synthesized";
      phase: "gathering" | "synthesizing" | "done";
      synthesizerModel: string;
      synthesizerInstanceId: string;
      completedModels: string[];
      totalModels: number;
      sourceResponses: SourceResponse[];
    }
  | {
      mode: "refined";
      phase: "initial" | "refining" | "done";
      currentRound: number;
      totalRounds: number;
      currentModel: string;
      rounds: RefinementRound[];
    }
  | {
      mode: "critiqued";
      phase: "initial" | "critiquing" | "revising" | "done";
      primaryModel: string;
      primaryInstanceId: string;
      initialResponse?: string;
      initialUsage?: MessageUsage;
      critiqueModels: string[];
      critiques: CritiqueData[];
      completedCritiques: number;
    }
  | {
      mode: "elected";
      phase: "responding" | "voting" | "done";
      candidates: CandidateResponse[];
      completedResponses: number;
      totalModels: number;
      votes: VoteData[];
      completedVotes: number;
      winner?: string;
      voteCounts?: Record<string, number>;
    }
  | {
      mode: "tournament";
      phase: "generating" | "competing" | "done";
      bracket: string[][];
      currentRound: number;
      totalRounds: number;
      matches: TournamentMatch[];
      currentMatch?: string;
      initialResponses: CandidateResponse[];
      eliminatedPerRound: string[][];
      winner?: string;
    }
  | {
      mode: "consensus";
      phase: "responding" | "revising" | "done";
      currentRound: number;
      maxRounds: number;
      threshold: number;
      rounds: ConsensusRound[];
      currentRoundResponses: CandidateResponse[];
      finalScore?: number;
    }
  | {
      mode: "debated";
      phase: "opening" | "debating" | "summarizing" | "done";
      currentRound: number;
      totalRounds: number;
      positions: Record<string, string>;
      turns: DebateTurn[];
      currentRoundTurns: DebateTurn[];
      summarizerModel: string;
      summarizerInstanceId: string;
      summary?: string;
      summaryUsage?: MessageUsage;
    }
  | {
      mode: "council";
      phase: "assigning" | "opening" | "discussing" | "synthesizing" | "done";
      currentRound: number;
      totalRounds: number;
      roles: Record<string, string>;
      statements: CouncilStatement[];
      currentRoundStatements: CouncilStatement[];
      synthesizerModel: string;
      synthesizerInstanceId: string;
      synthesis?: string;
      synthesisUsage?: MessageUsage;
    }
  | {
      mode: "hierarchical";
      phase: "decomposing" | "executing" | "synthesizing" | "done";
      coordinatorModel: string;
      coordinatorInstanceId: string;
      subtasks: HierarchicalSubtask[];
      workerResults: HierarchicalWorkerResult[];
      synthesis?: string;
      decompositionUsage?: MessageUsage;
      synthesisUsage?: MessageUsage;
    }
  | {
      mode: "scattershot";
      phase: "generating" | "done";
      targetModel: string;
      variations: ScattershotVariation[];
    }
  | {
      mode: "explainer";
      phase: "initial" | "simplifying" | "done";
      audienceLevels: string[];
      currentLevelIndex: number;
      explanations: ExplanationLevel[];
      currentModel?: string;
    }
  | {
      mode: "confidence-weighted";
      phase: "responding" | "synthesizing" | "done";
      responses: ConfidenceResponse[];
      completedResponses: number;
      totalModels: number;
      synthesizerModel: string;
      synthesizerInstanceId: string;
      synthesis?: string;
      synthesisUsage?: MessageUsage;
    };

interface StreamingState {
  /**
   * Map of instance ID to streaming response.
   * Instance IDs uniquely identify each stream (supports multiple instances of same model).
   * For simple cases, instance ID equals model ID.
   */
  streams: Map<string, StreamingResponse>;
  /** Whether any model is currently streaming */
  isStreaming: boolean;
  /**
   * Unified mode state using discriminated union.
   * This consolidates all mode-specific state into a single field.
   * Access mode-specific state via type narrowing on the `mode` discriminator.
   */
  modeState: ActiveModeState;
}

interface StreamingActions {
  /**
   * Initialize streaming responses for a set of instances.
   * @param instanceIds - Array of instance IDs (or model IDs for simple cases)
   * @param modelMap - Optional map from instance ID to model ID (for multi-instance scenarios)
   */
  initStreaming: (instanceIds: string[], modelMap?: Map<string, string>) => void;
  /** Append content to a specific instance's stream */
  appendContent: (instanceId: string, delta: string) => void;
  /** Set the full content for an instance (used for corrections/final text) */
  setContent: (instanceId: string, content: string) => void;
  /** Append reasoning content to a specific instance's stream */
  appendReasoningContent: (instanceId: string, delta: string) => void;
  /** Set the full reasoning content for an instance */
  setReasoningContent: (instanceId: string, content: string) => void;
  /** Push a completed round, then reset reasoningContent and content for the next round */
  pushCompletedRound: (instanceId: string, round: CompletedRound) => void;
  /** Attach tool execution data to the last completed round */
  setCompletedRoundToolExecution: (instanceId: string, toolExecution: ToolExecutionRound) => void;
  /**
   * Update an instance's running usage mid-stream (cumulative totals from
   * `response.usage.updated` events at server-tool turn boundaries).
   */
  updateStreamUsage: (instanceId: string, usage: MessageUsage) => void;
  /** Mark an instance's stream as complete */
  completeStream: (instanceId: string, usage?: MessageUsage) => void;
  /** Resume streaming for an instance (e.g., between tool-calling rounds) */
  resumeStreaming: (instanceId: string) => void;
  /** Set an error for an instance's stream */
  setError: (instanceId: string, error: string) => void;
  /** Clear all streams and reset mode state */
  clearStreams: () => void;
  /** Stop all streaming (mark as not streaming and reset mode state) */
  stopStreaming: () => void;
  /**
   * Set the unified mode state using discriminated union.
   * Pass the full state object with mode discriminator.
   * Automatically sets isStreaming based on whether phase is "done".
   */
  setModeState: (state: ActiveModeState) => void;
  /**
   * Update the current mode state with partial updates.
   * Only works if the mode matches the current mode (type-safe via callback).
   * Use this for incremental updates within a mode (e.g., adding a response).
   */
  updateModeState: <T extends ActiveModeState>(updater: (current: T) => T) => void;

  // Tool call management actions (for client-side tool execution)
  /** Add a new tool call to a model's stream */
  addToolCall: (model: string, toolCall: ToolCallState) => void;
  /** Update arguments buffer for a tool call (append delta) */
  updateToolCallArguments: (model: string, toolCallId: string, delta: string) => void;
  /** Mark a tool call as complete with parsed arguments */
  completeToolCall: (
    model: string,
    toolCallId: string,
    parsedArguments: Record<string, unknown>
  ) => void;
  /** Set error status on a tool call */
  setToolCallError: (model: string, toolCallId: string, error: string) => void;
  /** Clear all tool calls for a model */
  clearToolCalls: (model: string) => void;

  // Citation management actions
  /** Add citations to a model's stream (appends to existing) */
  addCitations: (model: string, citations: Citation[]) => void;
  /** Set all citations for a model (replaces existing) */
  setCitations: (model: string, citations: Citation[]) => void;
  /** Clear citations for a model */
  clearCitations: (model: string) => void;

  // Gateway MCP approval actions
  /** Append a pending gateway MCP approval request to a model's stream */
  addMcpApproval: (model: string, approval: McpApprovalRequest) => void;

  // Artifact management actions
  /** Add artifacts to a model's stream (appends to existing) */
  addArtifacts: (model: string, artifacts: Artifact[]) => void;
  /** Set all artifacts for a model (replaces existing) */
  setArtifacts: (model: string, artifacts: Artifact[]) => void;
  /** Clear artifacts for a model */
  clearArtifacts: (model: string) => void;

  // Tool execution timeline actions
  /** Start a new execution round for a model */
  startExecutionRound: (model: string) => number;
  /** Add a tool execution to the current round */
  addToolExecution: (model: string, execution: ToolExecution) => void;
  /** Update the status of a tool execution */
  updateToolExecutionStatus: (
    model: string,
    executionId: string,
    status: ToolExecution["status"],
    error?: string
  ) => void;
  /** Complete a tool execution with timing and artifacts */
  completeToolExecution: (
    model: string,
    executionId: string,
    inputArtifacts: Artifact[],
    outputArtifacts: Artifact[],
    error?: string
  ) => void;
  /** Set the status message for a tool execution (e.g., "Loading Python runtime...") */
  setToolExecutionStatusMessage: (model: string, executionId: string, message: string) => void;
  /** Set the model's reasoning for the current round (between tool calls) */
  setRoundModelReasoning: (model: string, reasoning: string) => void;
  /** Clear all tool execution rounds for a model */
  clearToolExecutionRounds: (model: string) => void;
}

export type StreamingStore = StreamingState & StreamingActions;

/**
 * Per-frame coalescing buffer for the high-frequency append paths.
 *
 * Token streaming can deliver 50–100+ tokens/sec per model. Each `set` call
 * clones the entire `streams: Map` to satisfy Zustand's referential-equality
 * change detection, so per-token writes produce O(N²·T) Map allocations where
 * N is the number of concurrent models and T the token rate.
 *
 * Instead of mutating the store on every delta, we accumulate deltas in a
 * module-level buffer and schedule a single `setState` per `requestAnimation
 * Frame`. Components see the same content trajectory but at frame cadence,
 * collapsing per-token Map clones into one per frame. Non-append operations
 * (`setContent`, `pushCompletedRound`, `clearStreams`, …) flush the buffer
 * synchronously before applying their authoritative update so a later
 * `setContent` can never lose intermediate appends to the buffer.
 */
type PendingDelta = {
  contentDelta: string;
  reasoningDelta: string;
  /** First-token capture time, recorded when the first delta arrives. */
  firstTokenTime: number | null;
};
const pendingDeltas: Map<string, PendingDelta> = new Map();
let rafHandle: number | null = null;

function getOrCreatePending(instanceId: string): PendingDelta {
  let entry = pendingDeltas.get(instanceId);
  if (!entry) {
    entry = { contentDelta: "", reasoningDelta: "", firstTokenTime: null };
    pendingDeltas.set(instanceId, entry);
  }
  return entry;
}

function scheduleFlush() {
  if (rafHandle !== null) return;
  if (typeof requestAnimationFrame === "undefined") {
    // Test/SSR environments — flush synchronously so callers see stable
    // behaviour (no rAF available to fire the deferred update).
    flushPendingDeltas();
    return;
  }
  rafHandle = requestAnimationFrame(() => {
    rafHandle = null;
    flushPendingDeltas();
  });
}

function flushPendingDeltas() {
  if (pendingDeltas.size === 0) return;
  // Snapshot and clear before mutating the store so any deltas that arrive
  // during the setState callback land in the next flush.
  const drained = new Map(pendingDeltas);
  pendingDeltas.clear();

  useStreamingStore.setState((state) => {
    const newStreams = new Map(state.streams);
    let changed = false;
    for (const [instanceId, pending] of drained) {
      const existing = newStreams.get(instanceId);
      if (!existing) continue;
      const isFirstToken = existing.content === "" && existing.reasoningContent === "";
      newStreams.set(instanceId, {
        ...existing,
        content: existing.content + pending.contentDelta,
        reasoningContent: existing.reasoningContent + pending.reasoningDelta,
        firstTokenTime:
          existing.firstTokenTime ??
          (isFirstToken ? (pending.firstTokenTime ?? Date.now()) : undefined),
      });
      changed = true;
    }
    return changed ? { streams: newStreams } : state;
  });
}

/**
 * Drop pending deltas for `instanceId` (or all instances if undefined).
 *
 * Call this from authoritative-overwrite operations so a queued append
 * doesn't get re-applied after a `setContent` resets the value.
 */
function discardPending(instanceId?: string): void {
  if (instanceId === undefined) {
    pendingDeltas.clear();
  } else {
    pendingDeltas.delete(instanceId);
  }
}

export const useStreamingStore = create<StreamingStore>((set) => ({
  streams: new Map(),
  isStreaming: false,
  modeState: { mode: null },

  initStreaming: (instanceIds, modelMap) =>
    set(() => {
      // Initialising a new round invalidates any pending coalesced deltas
      // from a previous round; otherwise stale deltas could land on a fresh
      // stream entry on the next rAF.
      discardPending();
      const streams = new Map<string, StreamingResponse>();
      const startTime = Date.now();
      for (const instanceId of instanceIds) {
        // Use modelMap to get the actual model ID, or fall back to instanceId
        const modelId = modelMap?.get(instanceId) ?? instanceId;
        streams.set(instanceId, {
          model: modelId,
          instanceId,
          content: "",
          reasoningContent: "",
          completedRounds: [],
          isStreaming: true,
          startTime,
        });
      }
      return { streams, isStreaming: true };
    }),

  appendContent: (model, delta) => {
    if (delta.length === 0) return;
    const pending = getOrCreatePending(model);
    pending.contentDelta += delta;
    if (pending.firstTokenTime === null) pending.firstTokenTime = Date.now();
    scheduleFlush();
  },

  setContent: (model, content) =>
    set((state) => {
      // Drop any pending deltas for this stream — the caller is overwriting
      // the value with an authoritative final string.
      discardPending(model);
      const existing = state.streams.get(model);
      if (!existing) return state;

      const newStreams = new Map(state.streams);
      newStreams.set(model, {
        ...existing,
        content,
      });
      return { streams: newStreams };
    }),

  appendReasoningContent: (model, delta) => {
    if (delta.length === 0) return;
    const pending = getOrCreatePending(model);
    pending.reasoningDelta += delta;
    if (pending.firstTokenTime === null) pending.firstTokenTime = Date.now();
    scheduleFlush();
  },

  setReasoningContent: (model, content) =>
    set((state) => {
      discardPending(model);
      const existing = state.streams.get(model);
      if (!existing) return state;

      const newStreams = new Map(state.streams);
      newStreams.set(model, {
        ...existing,
        reasoningContent: content,
      });
      return { streams: newStreams };
    }),

  pushCompletedRound: (model, round) =>
    set((state) => {
      // Round boundaries reset content/reasoning, so any unflushed appends
      // belong to the round being committed and must be applied first.
      flushPendingDeltas();
      discardPending(model);
      const existing = state.streams.get(model);
      if (!existing) return state;

      const newStreams = new Map(state.streams);
      newStreams.set(model, {
        ...existing,
        completedRounds: [...existing.completedRounds, round],
        reasoningContent: "",
        content: "",
      });
      return { streams: newStreams };
    }),

  setCompletedRoundToolExecution: (model, toolExecution) =>
    set((state) => {
      const existing = state.streams.get(model);
      if (!existing || existing.completedRounds.length === 0) return state;

      const rounds = [...existing.completedRounds];
      rounds[rounds.length - 1] = { ...rounds[rounds.length - 1], toolExecution };
      const newStreams = new Map(state.streams);
      newStreams.set(model, { ...existing, completedRounds: rounds });
      return { streams: newStreams };
    }),

  updateStreamUsage: (model, usage) =>
    set((state) => {
      const existing = state.streams.get(model);
      if (!existing) return state;

      const newStreams = new Map(state.streams);
      newStreams.set(model, { ...existing, usage });
      return { streams: newStreams };
    }),

  completeStream: (model, usage) =>
    set((state) => {
      // Apply any unflushed deltas before marking complete so the final
      // content visible to consumers includes the trailing tokens.
      flushPendingDeltas();
      const existing = state.streams.get(model);
      if (!existing) return state;

      const newStreams = new Map(state.streams);
      newStreams.set(model, {
        ...existing,
        isStreaming: false,
        usage,
      });

      // Check if all streams are complete
      const allComplete = Array.from(newStreams.values()).every((s) => !s.isStreaming);

      return {
        streams: newStreams,
        isStreaming: !allComplete,
      };
    }),

  resumeStreaming: (model) =>
    set((state) => {
      const existing = state.streams.get(model);
      if (!existing) return state;

      const newStreams = new Map(state.streams);
      newStreams.set(model, { ...existing, isStreaming: true });
      return { streams: newStreams, isStreaming: true };
    }),

  setError: (model, error) =>
    set((state) => {
      const existing = state.streams.get(model);
      if (!existing) return state;

      const newStreams = new Map(state.streams);
      newStreams.set(model, {
        ...existing,
        error,
        isStreaming: false,
      });

      // Check if all streams are complete
      const allComplete = Array.from(newStreams.values()).every((s) => !s.isStreaming);

      return {
        streams: newStreams,
        isStreaming: !allComplete,
      };
    }),

  clearStreams: () =>
    set(() => ({
      streams: new Map(),
      isStreaming: false,
      modeState: { mode: null },
    })),

  stopStreaming: () =>
    set((state) => {
      const newStreams = new Map(state.streams);
      for (const [model, stream] of newStreams) {
        if (stream.isStreaming) {
          newStreams.set(model, { ...stream, isStreaming: false });
        }
      }
      return {
        streams: newStreams,
        isStreaming: false,
        modeState: { mode: null },
      };
    }),

  setModeState: (modeState) =>
    set(() => {
      // Determine if streaming based on mode state
      // Mode is "done" streaming if phase is "done" or mode is null
      let isStreaming = true;
      if (modeState.mode === null) {
        isStreaming = false;
      } else if (modeState.mode === "chained") {
        // Chained mode doesn't have a phase - streaming continues until cleared
        isStreaming = true;
      } else if ("phase" in modeState && modeState.phase === "done") {
        isStreaming = false;
      }
      return { modeState, isStreaming };
    }),

  updateModeState: (updater) =>
    set((state) => {
      // Apply the updater function to the current mode state
      // The updater is responsible for type checking via the generic constraint
      const newModeState = updater(state.modeState as Parameters<typeof updater>[0]);

      // Determine if streaming based on updated mode state
      let isStreaming = true;
      if (newModeState.mode === null) {
        isStreaming = false;
      } else if (newModeState.mode === "chained") {
        isStreaming = true;
      } else if ("phase" in newModeState && newModeState.phase === "done") {
        isStreaming = false;
      }
      return { modeState: newModeState, isStreaming };
    }),

  // Tool call management implementations
  addToolCall: (model, toolCall) =>
    set((state) => {
      const existing = state.streams.get(model);
      if (!existing) return state;

      const toolCalls = new Map(existing.toolCalls ?? new Map());
      toolCalls.set(toolCall.id, toolCall);

      const newStreams = new Map(state.streams);
      newStreams.set(model, { ...existing, toolCalls });
      return { streams: newStreams };
    }),

  updateToolCallArguments: (model, toolCallId, delta) =>
    set((state) => {
      const existing = state.streams.get(model);
      if (!existing?.toolCalls) return state;

      const toolCall = existing.toolCalls.get(toolCallId);
      if (!toolCall) return state;

      const toolCalls = new Map(existing.toolCalls);
      toolCalls.set(toolCallId, {
        ...toolCall,
        argumentsBuffer: toolCall.argumentsBuffer + delta,
        status: toolCall.status === "pending" ? "executing" : toolCall.status,
      });

      const newStreams = new Map(state.streams);
      newStreams.set(model, { ...existing, toolCalls });
      return { streams: newStreams };
    }),

  completeToolCall: (model, toolCallId, parsedArguments) =>
    set((state) => {
      const existing = state.streams.get(model);
      if (!existing?.toolCalls) return state;

      const toolCall = existing.toolCalls.get(toolCallId);
      if (!toolCall) return state;

      const toolCalls = new Map(existing.toolCalls);
      toolCalls.set(toolCallId, {
        ...toolCall,
        status: "completed",
        parsedArguments,
      });

      const newStreams = new Map(state.streams);
      newStreams.set(model, { ...existing, toolCalls });
      return { streams: newStreams };
    }),

  setToolCallError: (model, toolCallId, error) =>
    set((state) => {
      const existing = state.streams.get(model);
      if (!existing?.toolCalls) return state;

      const toolCall = existing.toolCalls.get(toolCallId);
      if (!toolCall) return state;

      const toolCalls = new Map(existing.toolCalls);
      toolCalls.set(toolCallId, {
        ...toolCall,
        status: "failed",
        error,
      });

      const newStreams = new Map(state.streams);
      newStreams.set(model, { ...existing, toolCalls });
      return { streams: newStreams };
    }),

  clearToolCalls: (model) =>
    set((state) => {
      const existing = state.streams.get(model);
      if (!existing) return state;

      const newStreams = new Map(state.streams);
      newStreams.set(model, { ...existing, toolCalls: undefined });
      return { streams: newStreams };
    }),

  // Citation management implementations
  addCitations: (model, citations) =>
    set((state) => {
      const existing = state.streams.get(model);
      if (!existing) return state;

      const existingCitations = existing.citations ?? [];
      const newCitations = [...existingCitations, ...citations];

      const newStreams = new Map(state.streams);
      newStreams.set(model, { ...existing, citations: newCitations });
      return { streams: newStreams };
    }),

  setCitations: (model, citations) =>
    set((state) => {
      const existing = state.streams.get(model);
      if (!existing) return state;

      const newStreams = new Map(state.streams);
      newStreams.set(model, { ...existing, citations });
      return { streams: newStreams };
    }),

  clearCitations: (model) =>
    set((state) => {
      const existing = state.streams.get(model);
      if (!existing) return state;

      const newStreams = new Map(state.streams);
      newStreams.set(model, { ...existing, citations: undefined });
      return { streams: newStreams };
    }),

  // Gateway MCP approval implementations
  addMcpApproval: (model, approval) =>
    set((state) => {
      const existing = state.streams.get(model);
      if (!existing) return state;

      const current = existing.mcpApprovals ?? [];
      // Dedupe by approvalRequestId so replays don't double-add.
      if (current.some((a) => a.approvalRequestId === approval.approvalRequestId)) {
        return state;
      }

      const newStreams = new Map(state.streams);
      newStreams.set(model, { ...existing, mcpApprovals: [...current, approval] });
      return { streams: newStreams };
    }),

  // Artifact management implementations
  addArtifacts: (model, artifacts) =>
    set((state) => {
      const existing = state.streams.get(model);
      if (!existing) return state;

      const existingArtifacts = existing.artifacts ?? [];
      const existingIds = new Set(existingArtifacts.map((a) => a.id));
      const deduped = artifacts.filter((a) => !existingIds.has(a.id));
      if (deduped.length === 0) return state;

      const newStreams = new Map(state.streams);
      newStreams.set(model, { ...existing, artifacts: [...existingArtifacts, ...deduped] });
      return { streams: newStreams };
    }),

  setArtifacts: (model, artifacts) =>
    set((state) => {
      const existing = state.streams.get(model);
      if (!existing) return state;

      const newStreams = new Map(state.streams);
      newStreams.set(model, { ...existing, artifacts });
      return { streams: newStreams };
    }),

  clearArtifacts: (model) =>
    set((state) => {
      const existing = state.streams.get(model);
      if (!existing) return state;

      const newStreams = new Map(state.streams);
      newStreams.set(model, { ...existing, artifacts: undefined });
      return { streams: newStreams };
    }),

  // Tool execution timeline implementations
  startExecutionRound: (model) => {
    let newRoundNumber = 1;
    set((state) => {
      const existing = state.streams.get(model);
      if (!existing) return state;

      const rounds = existing.toolExecutionRounds ?? [];
      newRoundNumber = rounds.length + 1;

      const newRound: ToolExecutionRound = {
        round: newRoundNumber,
        executions: [],
        hasError: false,
      };

      const newStreams = new Map(state.streams);
      newStreams.set(model, {
        ...existing,
        toolExecutionRounds: [...rounds, newRound],
      });
      return { streams: newStreams };
    });
    return newRoundNumber;
  },

  addToolExecution: (model, execution) =>
    set((state) => {
      const existing = state.streams.get(model);
      if (!existing?.toolExecutionRounds?.length) return state;

      const rounds = [...existing.toolExecutionRounds];
      const currentRound = rounds[rounds.length - 1];

      // Add execution to current round
      rounds[rounds.length - 1] = {
        ...currentRound,
        executions: [...currentRound.executions, execution],
      };

      const newStreams = new Map(state.streams);
      newStreams.set(model, { ...existing, toolExecutionRounds: rounds });
      return { streams: newStreams };
    }),

  updateToolExecutionStatus: (model, executionId, status, error) =>
    set((state) => {
      const existing = state.streams.get(model);
      if (!existing?.toolExecutionRounds?.length) return state;

      const rounds = existing.toolExecutionRounds.map((round) => ({
        ...round,
        executions: round.executions.map((exec) =>
          exec.id === executionId ? { ...exec, status, error: error ?? exec.error } : exec
        ),
        hasError:
          round.hasError ||
          (status === "error" && round.executions.some((e) => e.id === executionId)),
      }));

      const newStreams = new Map(state.streams);
      newStreams.set(model, { ...existing, toolExecutionRounds: rounds });
      return { streams: newStreams };
    }),

  completeToolExecution: (model, executionId, inputArtifacts, outputArtifacts, error) =>
    set((state) => {
      const existing = state.streams.get(model);
      if (!existing?.toolExecutionRounds?.length) return state;

      const endTime = Date.now();
      const rounds = existing.toolExecutionRounds.map((round) => {
        const updatedExecutions = round.executions.map((exec) => {
          if (exec.id !== executionId) return exec;
          const duration = endTime - exec.startTime;
          return {
            ...exec,
            status: (error ? "error" : "success") as ToolExecution["status"],
            endTime,
            duration,
            inputArtifacts,
            outputArtifacts,
            error,
          };
        });

        // Calculate total duration and error status for the round
        const totalDuration = updatedExecutions.reduce((sum, e) => sum + (e.duration ?? 0), 0);
        const hasError = updatedExecutions.some((e) => e.status === "error");

        return {
          ...round,
          executions: updatedExecutions,
          totalDuration,
          hasError,
        };
      });

      const newStreams = new Map(state.streams);
      newStreams.set(model, { ...existing, toolExecutionRounds: rounds });
      return { streams: newStreams };
    }),

  setToolExecutionStatusMessage: (model, executionId, message) =>
    set((state) => {
      const existing = state.streams.get(model);
      if (!existing?.toolExecutionRounds?.length) return state;

      // Find and update the execution across all rounds
      const rounds = existing.toolExecutionRounds.map((round) => ({
        ...round,
        executions: round.executions.map((exec) =>
          exec.id === executionId ? { ...exec, statusMessage: message } : exec
        ),
      }));

      const newStreams = new Map(state.streams);
      newStreams.set(model, { ...existing, toolExecutionRounds: rounds });
      return { streams: newStreams };
    }),

  setRoundModelReasoning: (model, reasoning) =>
    set((state) => {
      const existing = state.streams.get(model);
      if (!existing?.toolExecutionRounds?.length) return state;

      const rounds = [...existing.toolExecutionRounds];
      const currentRound = rounds[rounds.length - 1];
      rounds[rounds.length - 1] = {
        ...currentRound,
        modelReasoning: reasoning,
      };

      const newStreams = new Map(state.streams);
      newStreams.set(model, { ...existing, toolExecutionRounds: rounds });
      return { streams: newStreams };
    }),

  clearToolExecutionRounds: (model) =>
    set((state) => {
      const existing = state.streams.get(model);
      if (!existing) return state;

      const newStreams = new Map(state.streams);
      newStreams.set(model, { ...existing, toolExecutionRounds: undefined });
      return { streams: newStreams };
    }),
}));

/**
 * Surgical Selectors - Prevent Unnecessary Re-renders
 *
 * These selectors are the key to performance. Each component should subscribe
 * to the minimum slice of state it needs:
 *
 * - `useStreamContent(instanceId)` - Single instance's content string only
 * - `useStreamState(instanceId)` - Single instance's full state object
 * - `useIsStreaming()` - Global boolean flag only
 * - `useAllStreams()` - All streams (use sparingly, with `useShallow`)
 *
 * NOTE: All selectors now use instance IDs as keys. For simple cases where
 * there's only one instance per model, instance ID equals model ID.
 *
 * IMPORTANT: Using `useStreamingStore(state => state.streams)` directly would
 * cause ALL subscribed components to re-render on ANY stream update. Always
 * use these surgical selectors instead.
 */

/**
 * Get streaming content for a specific instance.
 * Only re-renders when that instance's content changes.
 * @param instanceId - Instance ID (or model ID for simple cases)
 */
export const useStreamContent = (instanceId: string) =>
  useStreamingStore((state) => state.streams.get(instanceId)?.content ?? "");

/**
 * Get streaming reasoning content for a specific instance.
 * Only re-renders when that instance's reasoning changes.
 * @param instanceId - Instance ID (or model ID for simple cases)
 */
export const useStreamReasoningContent = (instanceId: string) =>
  useStreamingStore((state) => state.streams.get(instanceId)?.reasoningContent ?? "");

/**
 * Get streaming state for a specific instance.
 * @param instanceId - Instance ID (or model ID for simple cases)
 */
export const useStreamState = (instanceId: string) =>
  useStreamingStore((state) => state.streams.get(instanceId));

/** Get whether any streaming is active */
export const useIsStreaming = () => useStreamingStore((state) => state.isStreaming);

/**
 * Get all streaming responses as an array.
 *
 * Uses `useShallow` for shallow comparison - re-renders only when array contents change.
 * Use this in ChatMessageList to render the streaming section.
 */
export const useAllStreams = () =>
  useStreamingStore(useShallow((state) => Array.from(state.streams.values())));

/**
 * Unified Mode State Selectors
 *
 * These selectors work with the new discriminated union `modeState` field.
 * They provide type-safe access to mode-specific state.
 */

/** Get the unified mode state (discriminated union) */
export const useModeState = () => useStreamingStore((state) => state.modeState);

/**
 * Get the active mode name, or null if no mode is active.
 * Useful for conditional rendering based on which mode is running.
 */
export const useActiveModeName = () => useStreamingStore((state) => state.modeState.mode);

/**
 * Mode-specific helper selectors that extract typed state from the discriminated union.
 * These return the mode-specific state if that mode is active, otherwise undefined.
 * Use these when you need type-safe access to a specific mode's state.
 */

/** Get chained mode state if active */
export const useActiveChainedState = () =>
  useStreamingStore((state) => (state.modeState.mode === "chained" ? state.modeState : undefined));

/** Get routed mode state if active */
export const useActiveRoutedState = () =>
  useStreamingStore((state) => (state.modeState.mode === "routed" ? state.modeState : undefined));

/** Get synthesized mode state if active */
export const useActiveSynthesizedState = () =>
  useStreamingStore((state) =>
    state.modeState.mode === "synthesized" ? state.modeState : undefined
  );

/** Get refined mode state if active */
export const useActiveRefinedState = () =>
  useStreamingStore((state) => (state.modeState.mode === "refined" ? state.modeState : undefined));

/** Get critiqued mode state if active */
export const useActiveCritiquedState = () =>
  useStreamingStore((state) =>
    state.modeState.mode === "critiqued" ? state.modeState : undefined
  );

/** Get elected mode state if active */
export const useActiveElectedState = () =>
  useStreamingStore((state) => (state.modeState.mode === "elected" ? state.modeState : undefined));

/** Get tournament mode state if active */
export const useActiveTournamentState = () =>
  useStreamingStore((state) =>
    state.modeState.mode === "tournament" ? state.modeState : undefined
  );

/** Get consensus mode state if active */
export const useActiveConsensusState = () =>
  useStreamingStore((state) =>
    state.modeState.mode === "consensus" ? state.modeState : undefined
  );

/** Get debated mode state if active */
export const useActiveDebatedState = () =>
  useStreamingStore((state) => (state.modeState.mode === "debated" ? state.modeState : undefined));

/** Get council mode state if active */
export const useActiveCouncilState = () =>
  useStreamingStore((state) => (state.modeState.mode === "council" ? state.modeState : undefined));

/** Get hierarchical mode state if active */
export const useActiveHierarchicalState = () =>
  useStreamingStore((state) =>
    state.modeState.mode === "hierarchical" ? state.modeState : undefined
  );

/** Get scattershot mode state if active */
export const useActiveScattershotState = () =>
  useStreamingStore((state) =>
    state.modeState.mode === "scattershot" ? state.modeState : undefined
  );

/** Get explainer mode state if active */
export const useActiveExplainerState = () =>
  useStreamingStore((state) =>
    state.modeState.mode === "explainer" ? state.modeState : undefined
  );

/** Get confidence-weighted mode state if active */
export const useActiveConfidenceWeightedState = () =>
  useStreamingStore((state) =>
    state.modeState.mode === "confidence-weighted" ? state.modeState : undefined
  );

/**
 * Tool Call Selectors
 *
 * These selectors provide access to tool call state during streaming.
 * Used for client-side tool execution (e.g., file_search).
 */

/** Get all tool calls for a specific model as an array */
export const useToolCalls = (model: string) =>
  useStreamingStore(
    useShallow((state) => {
      const stream = state.streams.get(model);
      if (!stream?.toolCalls) return [];
      return Array.from(stream.toolCalls.values());
    })
  );

/** Get pending/executing tool calls for a specific model */
export const usePendingToolCalls = (model: string) =>
  useStreamingStore(
    useShallow((state) => {
      const stream = state.streams.get(model);
      if (!stream?.toolCalls) return [];
      return Array.from(stream.toolCalls.values()).filter(
        (tc) => tc.status === "pending" || tc.status === "executing"
      );
    })
  );

/** Check if a model has any active (pending/executing) tool calls */
export const useHasActiveToolCalls = (model: string) =>
  useStreamingStore((state) => {
    const stream = state.streams.get(model);
    if (!stream?.toolCalls) return false;
    return Array.from(stream.toolCalls.values()).some(
      (tc) => tc.status === "pending" || tc.status === "executing"
    );
  });

/** Get completed tool calls ready for execution */
export const useCompletedToolCalls = (model: string) =>
  useStreamingStore(
    useShallow((state) => {
      const stream = state.streams.get(model);
      if (!stream?.toolCalls) return [];
      return Array.from(stream.toolCalls.values()).filter(
        (tc) => tc.status === "completed" && tc.parsedArguments
      );
    })
  );

/** Get a specific tool call by ID */
export const useToolCall = (model: string, toolCallId: string) =>
  useStreamingStore((state) => {
    const stream = state.streams.get(model);
    return stream?.toolCalls?.get(toolCallId);
  });

/**
 * Citation Selectors
 *
 * These selectors provide access to citations from file_search/web_search results.
 */

/** Get citations for a specific model */
export const useCitations = (model: string) =>
  useStreamingStore(
    useShallow((state) => {
      const stream = state.streams.get(model);
      return stream?.citations ?? [];
    })
  );

/** Check if a model has any citations */
export const useHasCitations = (model: string) =>
  useStreamingStore((state) => {
    const stream = state.streams.get(model);
    return (stream?.citations?.length ?? 0) > 0;
  });

/**
 * Artifact Selectors
 *
 * These selectors provide access to artifacts from tool execution.
 */

/** Get artifacts for a specific model */
export const useArtifacts = (model: string) =>
  useStreamingStore(
    useShallow((state) => {
      const stream = state.streams.get(model);
      return stream?.artifacts ?? [];
    })
  );

/** Check if a model has any artifacts */
export const useHasArtifacts = (model: string) =>
  useStreamingStore((state) => {
    const stream = state.streams.get(model);
    return (stream?.artifacts?.length ?? 0) > 0;
  });

/**
 * Tool Execution Round Selectors
 *
 * These selectors provide access to tool execution timeline data.
 * Used by ToolExecutionBlock and related components for progressive disclosure UI.
 */

/** Get all tool execution rounds for a specific model */
export const useToolExecutionRounds = (model: string) =>
  useStreamingStore(
    useShallow((state) => {
      const stream = state.streams.get(model);
      return stream?.toolExecutionRounds ?? [];
    })
  );

/** Get the current (most recent) execution round for a model */
export const useCurrentExecutionRound = (model: string) =>
  useStreamingStore((state) => {
    const stream = state.streams.get(model);
    const rounds = stream?.toolExecutionRounds;
    return rounds?.length ? rounds[rounds.length - 1] : undefined;
  });

/** Check if a model has any tool execution rounds */
export const useHasToolExecutionRounds = (model: string) =>
  useStreamingStore((state) => {
    const stream = state.streams.get(model);
    return (stream?.toolExecutionRounds?.length ?? 0) > 0;
  });

/** Get total number of executions across all rounds for a model */
export const useTotalToolExecutions = (model: string) =>
  useStreamingStore((state) => {
    const stream = state.streams.get(model);
    if (!stream?.toolExecutionRounds) return 0;
    return stream.toolExecutionRounds.reduce((sum, r) => sum + r.executions.length, 0);
  });

/** Check if any execution is currently running for a model */
export const useHasRunningExecution = (model: string) =>
  useStreamingStore((state) => {
    const stream = state.streams.get(model);
    if (!stream?.toolExecutionRounds) return false;
    return stream.toolExecutionRounds.some((r) => r.executions.some((e) => e.status === "running"));
  });

/** Get the status message of the currently running execution (if any) */
export const useRunningExecutionStatusMessage = (model: string) =>
  useStreamingStore((state) => {
    const stream = state.streams.get(model);
    if (!stream?.toolExecutionRounds) return undefined;
    for (const round of stream.toolExecutionRounds) {
      for (const exec of round.executions) {
        if (exec.status === "running" && exec.statusMessage) return exec.statusMessage;
      }
    }
    return undefined;
  });
