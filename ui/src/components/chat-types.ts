// Import and re-export Artifact types from toolExecutors for use in other modules
import type {
  Artifact as ArtifactImport,
  ArtifactType as ArtifactTypeImport,
  ArtifactRole as ArtifactRoleImport,
  CodeArtifactData as CodeArtifactDataImport,
  TableArtifactData as TableArtifactDataImport,
  ChartArtifactData as ChartArtifactDataImport,
  DisplaySelectionData as DisplaySelectionDataImport,
  AgentArtifactData as AgentArtifactDataImport,
  ContainerFileArtifactData as ContainerFileArtifactDataImport,
  FileSearchArtifactData as FileSearchArtifactDataImport,
  FileSearchResultItem as FileSearchResultItemImport,
  ToolExecutionStatus as ToolExecutionStatusImport,
  ToolExecution as ToolExecutionImport,
  ToolExecutionRound as ToolExecutionRoundImport,
} from "@/pages/chat/utils/toolExecutors";

export type Artifact = ArtifactImport;
export type ArtifactType = ArtifactTypeImport;
export type ArtifactRole = ArtifactRoleImport;
export type CodeArtifactData = CodeArtifactDataImport;
export type TableArtifactData = TableArtifactDataImport;
export type ChartArtifactData = ChartArtifactDataImport;
export type DisplaySelectionData = DisplaySelectionDataImport;
export type AgentArtifactData = AgentArtifactDataImport;
export type ContainerFileArtifactData = ContainerFileArtifactDataImport;
export type FileSearchArtifactData = FileSearchArtifactDataImport;
export type FileSearchResultItem = FileSearchResultItemImport;
export type ToolExecutionStatus = ToolExecutionStatusImport;
export type ToolExecution = ToolExecutionImport;
export type ToolExecutionRound = ToolExecutionRoundImport;

/** A completed round of multi-round tool execution, bundling reasoning, content, and tool execution */
export interface CompletedRound {
  reasoning?: string;
  content?: string;
  toolExecution?: ToolExecutionRound;
}

/** History mode for conversation context sent to models */
export type HistoryMode = "all" | "same-model";

/**
 * Conversation modes define how multiple models interact when responding to prompts.
 *
 * Phase 1 - Core Modes:
 * - multiple: Each model responds independently in parallel (current default)
 * - chained: Models respond sequentially, each seeing previous responses
 * - routed: A router model selects which model should respond
 *
 * Phase 2 - Synthesis Modes:
 * - synthesized: All models respond, then a synthesizer combines results
 * - refined: Models take turns refining a response
 * - critiqued: One responds, others critique, original revises
 *
 * Phase 3 - Competitive Modes:
 * - elected: Models vote on the best response
 * - tournament: Models compete in brackets
 * - consensus: Models revise until agreement
 *
 * Phase 4 - Advanced Modes:
 * - debated: Models argue back and forth
 * - council: Models discuss from assigned perspectives
 * - hierarchical: One model delegates to others
 *
 * Phase 5 - Experimental Modes:
 * - alloyed: Interleave tokens from multiple models
 * - scattershot: Same model, different parameters
 * - confidence-weighted: Weight responses by confidence
 * - evolutionary: Genetic operations on responses
 * - explainer: Progressive simplification
 */
export type ConversationMode =
  // Phase 1 - Core Modes
  | "multiple"
  | "chained"
  | "routed"
  // Phase 2 - Synthesis Modes
  | "synthesized"
  | "refined"
  | "critiqued"
  // Phase 3 - Competitive Modes
  | "elected"
  | "tournament"
  | "consensus"
  // Phase 4 - Advanced Modes
  | "debated"
  | "council"
  | "hierarchical"
  // Phase 5 - Experimental Modes
  | "alloyed"
  | "scattershot"
  | "confidence-weighted"
  | "evolutionary"
  | "explainer";

/** Configuration specific to each conversation mode */
export interface ModeConfig {
  /** Chained mode: order of models (defaults to selection order) */
  chainOrder?: string[];

  /** Routed mode: which model acts as router (defaults to first selected) */
  routerModel?: string;
  /** Routed mode: which instance acts as router (takes precedence over routerModel) */
  routerInstanceId?: string;
  /** Routed mode: custom routing prompt */
  routingPrompt?: string;

  /** Synthesized mode: which model synthesizes results (defaults to first selected) */
  synthesizerModel?: string;
  /** Synthesized mode: which instance synthesizes results (takes precedence over synthesizerModel) */
  synthesizerInstanceId?: string;
  /** Synthesized mode: custom synthesis prompt */
  synthesisPrompt?: string;

  /** Refined mode: number of refinement rounds */
  refinementRounds?: number;
  /** Refined mode: custom refinement prompt */
  refinementPrompt?: string;

  /** Critiqued mode: which model provides initial response (defaults to first) */
  primaryModel?: string;
  /** Critiqued mode: which instance provides initial response (takes precedence over primaryModel) */
  primaryInstanceId?: string;
  /** Critiqued mode: custom critique prompt */
  critiquePrompt?: string;

  /** Elected mode: custom voting prompt */
  votingPrompt?: string;

  /** Debated mode: number of debate rounds */
  debateRounds?: number;
  /** Debated mode: custom debate prompt */
  debatePrompt?: string;

  /** Council mode: role assignments per model */
  councilRoles?: Record<string, string>;
  /** Council mode: custom council prompt */
  councilPrompt?: string;
  /** Council mode: let the first model auto-assign roles based on the question */
  councilAutoAssignRoles?: boolean;

  /** Alloyed mode: tokens per turn before switching */
  tokensPerTurn?: number;

  /** Scattershot mode: parameter variations to try */
  parameterVariations?: ModelParameters[];

  /** Tournament mode: bracket structure (auto-generated if not provided) */
  tournamentBracket?: string[][];

  /** Consensus mode: maximum rounds before forced conclusion */
  maxConsensusRounds?: number;
  /** Consensus mode: agreement threshold (0-1) */
  consensusThreshold?: number;
  /** Consensus mode: custom consensus prompt */
  consensusPrompt?: string;

  /** Evolutionary mode: population size */
  populationSize?: number;
  /** Evolutionary mode: number of generations */
  generations?: number;
  /** Evolutionary mode: mutation rate */
  mutationRate?: number;

  /** Hierarchical mode: which model is the coordinator */
  coordinatorModel?: string;
  /** Hierarchical mode: which instance is the coordinator (takes precedence over coordinatorModel) */
  coordinatorInstanceId?: string;
  /** Hierarchical mode: custom worker prompt */
  hierarchicalWorkerPrompt?: string;

  /** Explainer mode: target audience levels */
  audienceLevels?: string[];

  /** Confidence-weighted mode: custom confidence response prompt */
  confidencePrompt?: string;
  /** Confidence-weighted mode: minimum confidence threshold to include in synthesis (0-1) */
  confidenceThreshold?: number;
}

/** Metadata about a conversation mode for UI display */
export interface ModeMetadata {
  id: ConversationMode;
  name: string;
  description: string;
  icon: string; // Lucide icon name
  phase: 1 | 2 | 3 | 4 | 5;
  minModels: number;
  maxModels?: number;
  /** Whether this mode is fully implemented */
  implemented: boolean;
}

/** All available conversation modes with metadata */
export const CONVERSATION_MODES: ModeMetadata[] = [
  // Phase 1 - Core Modes
  {
    id: "multiple",
    name: "Multiple",
    description: "Each model responds independently in parallel",
    icon: "LayoutGrid",
    phase: 1,
    minModels: 1,
    implemented: true,
  },
  {
    id: "chained",
    name: "Chained",
    description: "Models respond sequentially, building on each other",
    icon: "Link",
    phase: 1,
    minModels: 2,
    implemented: true,
  },
  {
    id: "routed",
    name: "Routed",
    description: "A router model selects the best model for each prompt",
    icon: "GitBranch",
    phase: 1,
    minModels: 2,
    implemented: true,
  },
  // Phase 2 - Synthesis Modes
  {
    id: "synthesized",
    name: "Synthesized",
    description: "All models respond, then one synthesizes the results",
    icon: "Combine",
    phase: 2,
    minModels: 2,
    implemented: true,
  },
  {
    id: "refined",
    name: "Refined",
    description: "Models take turns improving a response",
    icon: "Sparkles",
    phase: 2,
    minModels: 2,
    implemented: true,
  },
  {
    id: "critiqued",
    name: "Critiqued",
    description: "One responds, others critique, then revision",
    icon: "MessageSquareWarning",
    phase: 2,
    minModels: 2,
    implemented: true,
  },
  // Phase 3 - Competitive Modes
  {
    id: "elected",
    name: "Elected",
    description: "Models vote to select the best response",
    icon: "Vote",
    phase: 3,
    minModels: 3,
    implemented: true,
  },
  {
    id: "tournament",
    name: "Tournament",
    description: "Models compete in elimination brackets",
    icon: "Trophy",
    phase: 3,
    minModels: 4,
    implemented: true,
  },
  {
    id: "consensus",
    name: "Consensus",
    description: "Models revise until they agree",
    icon: "Handshake",
    phase: 3,
    minModels: 2,
    implemented: true,
  },
  // Phase 4 - Advanced Modes
  {
    id: "debated",
    name: "Debated",
    description: "Models argue different positions",
    icon: "Swords",
    phase: 4,
    minModels: 2,
    implemented: true,
  },
  {
    id: "council",
    name: "Council",
    description: "Models discuss from assigned perspectives",
    icon: "Users",
    phase: 4,
    minModels: 2,
    implemented: true,
  },
  {
    id: "hierarchical",
    name: "Hierarchical",
    description: "One model coordinates, others execute",
    icon: "Network",
    phase: 4,
    minModels: 2,
    implemented: true,
  },
  // Phase 5 - Experimental Modes
  {
    id: "alloyed",
    name: "Alloyed",
    description: "Interleave tokens from multiple models",
    icon: "Shuffle",
    phase: 5,
    minModels: 2,
    implemented: false,
  },
  {
    id: "scattershot",
    name: "Scattershot",
    description: "Same prompt with varied parameters",
    icon: "Target",
    phase: 5,
    minModels: 1,
    implemented: true,
  },
  {
    id: "confidence-weighted",
    name: "Confidence",
    description: "Weight responses by model confidence",
    icon: "Scale",
    phase: 5,
    minModels: 2,
    implemented: true,
  },
  {
    id: "evolutionary",
    name: "Evolutionary",
    description: "Evolve responses through generations",
    icon: "Dna",
    phase: 5,
    minModels: 2,
    implemented: false,
  },
  {
    id: "explainer",
    name: "Explainer",
    description: "Progressive simplification for audiences",
    icon: "GraduationCap",
    phase: 5,
    minModels: 1,
    implemented: true,
  },
];

/** Get mode metadata by ID */
export function getModeMetadata(mode: ConversationMode): ModeMetadata {
  const metadata = CONVERSATION_MODES.find((m) => m.id === mode);
  if (!metadata) {
    throw new Error(`Unknown conversation mode: ${mode}`);
  }
  return metadata;
}

/** Default mode configuration */
export const DEFAULT_MODE_CONFIG: ModeConfig = {
  refinementRounds: 2,
  debateRounds: 3,
  tokensPerTurn: 50,
  maxConsensusRounds: 5,
  consensusThreshold: 0.8,
  populationSize: 4,
  generations: 3,
  mutationRate: 0.1,
  audienceLevels: ["expert", "intermediate", "beginner"],
};

/**
 * Reasoning effort level for models that support extended thinking.
 *
 * `xhigh` and `max` are accepted for every model; providers clamp them down to
 * `high` where the higher levels aren't supported.
 */
export type ReasoningEffort = "none" | "minimal" | "low" | "medium" | "high" | "xhigh" | "max";

/** Reasoning configuration for chat requests */
export interface ReasoningConfig {
  /** Whether reasoning is enabled */
  enabled: boolean;
  /** Effort level for reasoning */
  effort: ReasoningEffort;
}

/** Default reasoning configuration - enabled with medium effort */
export const DEFAULT_REASONING_CONFIG: ReasoningConfig = {
  enabled: true,
  effort: "medium",
};

/** Token usage and cost information for a message */
export interface MessageUsage {
  inputTokens: number;
  outputTokens: number;
  totalTokens: number;
  /** Cost in dollars (from provider or calculated) */
  cost?: number;
  /** Cached tokens count (if applicable) */
  cachedTokens?: number;
  /** Reasoning tokens count (if applicable) */
  reasoningTokens?: number;
  /** Reasoning content (extended thinking output — last/only round) */
  reasoningContent?: string;

  // Timing stats (captured client-side during streaming)
  /** Time to first token in milliseconds (from request start) */
  firstTokenMs?: number;
  /** Total response duration in milliseconds (from request start to completion) */
  totalDurationMs?: number;
  /** Tokens per second (output tokens / duration in seconds) */
  tokensPerSecond?: number;

  // Response metadata
  /** Why the response ended (stop, length, tool_use, content_filter, etc.) */
  finishReason?: string;
  /** Exact model ID/version string from the response */
  modelId?: string;
  /** Provider that served the request */
  provider?: string;
}

/** Single refinement round result (for storing in message metadata) */
export interface RefinementRoundData {
  /** The model that performed this refinement */
  model: string;
  /** The content after this refinement */
  content: string;
  /** Token usage for this round */
  usage?: MessageUsage;
}

/** Single critique data (for storing in message metadata) */
export interface CritiqueRoundData {
  /** The model that provided this critique */
  model: string;
  /** The critique content */
  content: string;
  /** Token usage for this critique */
  usage?: MessageUsage;
}

/** Candidate response data (for storing in elected mode metadata) */
export interface CandidateData {
  /** The model that generated this response */
  model: string;
  /** The response content */
  content: string;
  /** Token usage for this response */
  usage?: MessageUsage;
}

/** Vote data (for storing in elected mode metadata) */
export interface VoteRoundData {
  /** The model that cast this vote */
  voter: string;
  /** The model being voted for */
  votedFor: string;
  /** Optional reasoning for the vote */
  reasoning?: string;
  /** Token usage for this vote */
  usage?: MessageUsage;
}

/** Single consensus round data (for storing in message metadata) */
export interface ConsensusRoundData {
  /** Round number (0 = initial) */
  round: number;
  /** Responses from all models in this round */
  responses: CandidateData[];
  /** Whether consensus was reached in this round */
  consensusReached: boolean;
  /** Consensus score for this round (0-1) */
  consensusScore?: number;
}

/** Single debate turn data (for storing in message metadata) */
export interface DebateTurnData {
  /** The model that made this argument */
  model: string;
  /** The position being argued (e.g., "pro", "con") */
  position: string;
  /** The argument content */
  content: string;
  /** Which round this turn belongs to (0 = opening) */
  round: number;
  /** Token usage for this turn */
  usage?: MessageUsage;
}

/** Single council statement data (for storing in message metadata) */
export interface CouncilStatementData {
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

/** Hierarchical subtask data (for storing in message metadata) */
export interface HierarchicalSubtaskData {
  /** Subtask identifier */
  id: string;
  /** Description of the subtask */
  description: string;
  /** Model assigned to this subtask */
  assignedModel: string;
  /** Status when completed */
  status: "pending" | "in_progress" | "complete" | "failed";
  /** Result content (if complete) */
  result?: string;
}

/** Hierarchical worker result data (for storing in message metadata) */
export interface HierarchicalWorkerResultData {
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

/** Scattershot variation data (for storing in message metadata) */
export interface ScattershotVariationData {
  /** Unique identifier for this variation */
  id: string;
  /** Index in the variations array */
  index: number;
  /** Parameter overrides for this variation */
  params: ModelParameters;
  /** Human-readable label for the variation */
  label: string;
  /** Response content */
  content?: string;
  /** Token usage */
  usage?: MessageUsage;
}

/** Single tournament match result */
export interface TournamentMatchData {
  /** Match ID (round-match format, e.g. "0-0" for first match of first round) */
  id: string;
  /** Round number (0-indexed) */
  round: number;
  /** Competitor 1 model ID */
  competitor1: string;
  /** Competitor 2 model ID */
  competitor2: string;
  /** The winning model */
  winner: string;
  /** The judge model that decided */
  judge: string;
  /** Optional reasoning from the judge */
  reasoning?: string;
  /** Competitor 1's response content */
  response1: string;
  /** Competitor 2's response content */
  response2: string;
  /** Usage from competitor 1 */
  usage1?: MessageUsage;
  /** Usage from competitor 2 */
  usage2?: MessageUsage;
  /** Usage from the judge */
  judgeUsage?: MessageUsage;
}

/** Metadata about how a message was generated in multi-model modes */
export interface MessageModeMetadata {
  /** The conversation mode used */
  mode: ConversationMode;
  /** For chained mode: position in the chain (0-indexed) */
  chainPosition?: number;
  /** For chained mode: total number of models in chain */
  chainTotal?: number;
  /** For routed mode: the model that acted as router */
  routerModel?: string;
  /** For routed mode: reasoning for the selection */
  routingReasoning?: string;
  /** For routed mode: usage from the router request (contributes to total cost) */
  routerUsage?: MessageUsage;
  /** For synthesized mode: whether this is the synthesized response */
  isSynthesized?: boolean;
  /** For synthesized mode: the model that synthesized the responses */
  synthesizerModel?: string;
  /** For synthesized mode: the individual responses that were synthesized */
  sourceResponses?: Array<{
    model: string;
    content: string;
    usage?: MessageUsage;
  }>;
  /** For synthesized mode: usage from the synthesis request (contributes to total cost) */
  synthesizerUsage?: MessageUsage;
  /** For refined mode: whether this is the final refined response */
  isRefined?: boolean;
  /** For refined mode: the round number of this refinement (0 = initial) */
  refinementRound?: number;
  /** For refined mode: total number of refinement rounds */
  totalRounds?: number;
  /** For refined mode: history of all refinement rounds including initial */
  refinementHistory?: RefinementRoundData[];
  /** For critiqued mode: whether this is the revised response */
  isCritiqued?: boolean;
  /** For critiqued mode: the model that provided initial response and revision */
  primaryModel?: string;
  /** For critiqued mode: the initial response before critiques */
  initialResponse?: string;
  /** For critiqued mode: usage from the initial response */
  initialUsage?: MessageUsage;
  /** For critiqued mode: the critiques from other models */
  critiques?: CritiqueRoundData[];
  /** For elected mode: whether this is the elected (winning) response */
  isElected?: boolean;
  /** For elected mode: the winning model */
  winner?: string;
  /** For elected mode: all candidate responses */
  candidates?: CandidateData[];
  /** For elected mode: all votes cast */
  votes?: VoteRoundData[];
  /** For elected mode: vote counts per candidate */
  voteCounts?: Record<string, number>;
  /** For elected mode: aggregate usage from voting */
  voteUsage?: MessageUsage;
  /** For tournament mode: whether this is the tournament winner's response */
  isTournamentWinner?: boolean;
  /** For tournament mode: the tournament bracket structure [[round1 matches], [round2 matches], ...] */
  bracket?: string[][];
  /** For tournament mode: all match results */
  matches?: TournamentMatchData[];
  /** For tournament mode: the final winner model */
  tournamentWinner?: string;
  /** For tournament mode: models eliminated in each round */
  eliminatedPerRound?: string[][];
  /** For consensus mode: whether this is the consensus response */
  isConsensus?: boolean;
  /** For consensus mode: the round in which consensus was reached or max rounds */
  consensusRound?: number;
  /** For consensus mode: whether consensus was actually reached */
  consensusReached?: boolean;
  /** For consensus mode: final consensus score (0-1) */
  consensusScore?: number;
  /** For consensus mode: all rounds of consensus building */
  rounds?: ConsensusRoundData[];
  /** For consensus mode: aggregate usage from all rounds */
  aggregateUsage?: MessageUsage;
  /** For debated mode: whether this is the debate summary */
  isDebateSummary?: boolean;
  /** For debated mode: model positions (model ID -> position name) */
  debatePositions?: Record<string, string>;
  /** For debated mode: all debate turns */
  debateTurns?: DebateTurnData[];
  /** For debated mode: number of debate rounds */
  debateRounds?: number;
  /** For debated mode: the model that provided the summary */
  summarizerModel?: string;
  /** For debated mode: usage from the summary request */
  summaryUsage?: MessageUsage;
  /** For council mode: whether this is the council synthesis */
  isCouncilSynthesis?: boolean;
  /** For council mode: model roles (model ID -> role name) */
  councilRoles?: Record<string, string>;
  /** For council mode: all council statements */
  councilStatements?: CouncilStatementData[];
  /** For council mode: number of discussion rounds */
  councilRounds?: number;
  /** For hierarchical mode: whether this is the hierarchical synthesis */
  isHierarchicalSynthesis?: boolean;
  /** For hierarchical mode: the coordinator model */
  coordinatorModel?: string;
  /** For hierarchical mode: subtask definitions */
  subtasks?: HierarchicalSubtaskData[];
  /** For hierarchical mode: all worker results */
  workerResults?: HierarchicalWorkerResultData[];
  /** For hierarchical mode: usage from decomposition step */
  decompositionUsage?: MessageUsage;
  /** For scattershot mode: whether this is a scattershot response */
  isScattershot?: boolean;
  /** For scattershot mode: the model used for all variations */
  scattershotModel?: string;
  /** For scattershot mode: the instance label (if using a named instance) */
  scattershotInstanceLabel?: string;
  /** For scattershot mode: all variation results */
  scattershotVariations?: ScattershotVariationData[];
  /** For scattershot mode: this variation's label (e.g., "temp=0.5") */
  scattershotVariationLabel?: string;
  /** For scattershot mode: this variation's parameters */
  scattershotVariationParams?: ModelParameters;
  /** For explainer mode: whether this is the multi-level explanation */
  isExplanation?: boolean;
  /** For explainer mode: the audience levels used */
  explainerLevels?: string[];
  /** For explainer mode: all explanations at different levels */
  explanations?: ExplanationData[];
  /** For explainer mode: this explanation's audience level */
  explainerLevel?: string;
  /** For explainer mode: the model that generated this explanation */
  explainerModel?: string;
  /** For explainer mode: the instance label (if using a named instance) */
  explainerInstanceLabel?: string;
  /** For confidence-weighted mode: whether this is the confidence-weighted synthesis */
  isConfidenceWeighted?: boolean;
  /** For confidence-weighted mode: all responses with confidence scores */
  confidenceResponses?: ConfidenceResponseData[];
}

/** Single explanation at a specific audience level (for storing in message metadata) */
export interface ExplanationData {
  /** The audience level (e.g., "expert", "intermediate", "beginner") */
  level: string;
  /** The model that generated this explanation */
  model: string;
  /** The instance label (if using a named instance) */
  instanceLabel?: string;
  /** The explanation content */
  content: string;
  /** Token usage for this explanation */
  usage?: MessageUsage;
}

/** Single confidence-weighted response data (for storing in message metadata) */
export interface ConfidenceResponseData {
  /** The model that generated this response */
  model: string;
  /** The response content */
  content: string;
  /** Self-assessed confidence score (0-1) */
  confidence: number;
  /** Token usage for this response */
  usage?: MessageUsage;
}

/** Types of citations that can be displayed */
export type CitationType = "file" | "url" | "chunk";

/** Base citation interface */
interface BaseCitation {
  /** Unique identifier */
  id: string;
  /** Type of citation */
  type: CitationType;
  /** Relevance score (0-1) if available */
  score?: number;
}

/** File citation from vector store search */
export interface FileCitation extends BaseCitation {
  type: "file";
  /** File ID in the vector store */
  fileId: string;
  /** Display filename */
  filename: string;
  /** Optional chunk ID within the file */
  chunkId?: string;
  /** Optional snippet of the content */
  snippet?: string;
  /** Character range in the original file */
  charRange?: { start: number; end: number };
}

/** URL citation from web search */
export interface UrlCitation extends BaseCitation {
  type: "url";
  /** The source URL */
  url: string;
  /** Page title */
  title: string;
  /** Optional snippet of the content */
  snippet?: string;
}

/** Chunk citation with full content preview */
export interface ChunkCitation extends BaseCitation {
  type: "chunk";
  /** File ID containing this chunk */
  fileId: string;
  /** Display filename */
  filename: string;
  /** Chunk index within the file */
  chunkIndex: number;
  /** Full chunk content */
  content: string;
  /** Token count of the chunk */
  tokenCount?: number;
}

/** Union type for all citation types */
export type Citation = FileCitation | UrlCitation | ChunkCitation;

/**
 * A gateway MCP tool call that paused for human approval
 * (`require_approval`). The user approves/denies it and the chat resumes the
 * response by echoing back an `mcp_approval_response` keyed by
 * `approvalRequestId`.
 */
export interface McpApprovalRequest {
  /** Output item id of the approval request. */
  id: string;
  /** Id echoed back in the `mcp_approval_response` to resume. */
  approvalRequestId: string;
  /** MCP server label that requested the call. */
  serverLabel: string;
  /** Tool the model wants to call. */
  toolName: string;
  /** Raw JSON arguments string (for display when not parseable). */
  argumentsJson: string;
  /** Parsed arguments, when the JSON was valid. */
  parsedArguments?: Record<string, unknown>;
  /** Set once the user responds; absent while still pending. */
  resolved?: "approved" | "denied";
}

export interface ChatMessage {
  id: string;
  role: "user" | "assistant" | "system";
  content: string;
  /** Model ID that generated this message (assistant messages only) */
  model?: string;
  /**
   * Instance ID that generated this message (assistant messages only).
   * For multi-instance scenarios, this uniquely identifies which instance
   * (e.g., "gpt-4-creative" vs "gpt-4-precise") generated the response.
   * If not set, defaults to model ID for backwards compatibility.
   */
  instanceId?: string;
  timestamp: Date;
  isStreaming?: boolean;
  files?: ChatFile[];
  /** Token usage and cost for this message (assistant messages only) */
  usage?: MessageUsage;
  /** User feedback for this message (assistant messages only) */
  feedback?: ResponseFeedbackData;
  /** History mode used when this message was sent (user messages only) */
  historyMode?: HistoryMode;
  /** Mode-specific metadata for multi-model modes */
  modeMetadata?: MessageModeMetadata;
  /** Error message if the request failed (assistant messages only) */
  error?: string;
  /** Citations from file_search or web_search tools (assistant messages only) */
  citations?: Citation[];
  /** Artifacts produced by tool execution (charts, tables, images, etc.) */
  artifacts?: Artifact[];
  /** Tool execution timeline for multi-turn tool calling (assistant messages only) */
  toolExecutionRounds?: ToolExecutionRound[];
  /** Completed rounds bundling reasoning, content, and tool execution (multi-round tool execution) */
  completedRounds?: CompletedRound[];
  /** Debug message ID for looking up debug info in debugStore (assistant messages only) */
  debugMessageId?: string;
  /** Gateway MCP approval requests this response paused on (assistant messages only) */
  pendingMcpApprovals?: McpApprovalRequest[];
}

export interface ChatFile {
  id: string;
  name: string;
  type: string;
  size: number;
  base64: string;
  preview?: string;
}

/**
 * A message the user composed while a response was still streaming. Queued
 * messages are sent one at a time as each in-flight turn completes (see the
 * queue drain in `ChatPage`).
 */
export interface QueuedMessage {
  id: string;
  content: string;
  files: ChatFile[];
}

export interface Conversation {
  id: string;
  /** Server-assigned ID after sync (may differ from local id) */
  remoteId?: string;
  title: string;
  messages: ChatMessage[];
  models: string[];
  createdAt: Date;
  updatedAt: Date;
  /** If set, this conversation is shared via a project */
  projectId?: string;
  /** Project name for display purposes */
  projectName?: string;
  /** Pin order: null/undefined = not pinned, 0-N = pinned with order (lower = higher in list) */
  pinOrder?: number | null;
  /** Usage from LLM-based title generation (if used) */
  titleGenerationUsage?: MessageUsage;
}

/** Feedback rating for a response */
export type ResponseFeedback = "positive" | "negative" | null;

/** Feedback data stored per response */
export interface ResponseFeedbackData {
  rating: ResponseFeedback;
  /** Whether this response was selected as the best in a comparison */
  selectedAsBest?: boolean;
}

export interface ModelResponse {
  model: string;
  content: string;
  isStreaming: boolean;
  error?: string;
  /** Usage data received from response.completed event */
  usage?: MessageUsage;
  /** User feedback for this response */
  feedback?: ResponseFeedbackData;
}

export interface ModelParameters {
  temperature?: number;
  maxTokens?: number;
  topP?: number;
  topK?: number;
  frequencyPenalty?: number;
  presencePenalty?: number;
  /** Reasoning configuration for this model */
  reasoning?: ReasoningConfig;
  /** Per-model system prompt override (takes precedence over conversation-wide system prompt) */
  systemPrompt?: string;
}

/**
 * A model instance represents a specific configuration of a model.
 * This allows the same model to be used multiple times with different settings
 * (e.g., compare GPT-4 with temperature 0.3 vs 0.9).
 */
export interface ModelInstance {
  /** Unique instance identifier (e.g., "gpt-4-creative", "gpt-4-precise") */
  id: string;
  /** Base model ID (e.g., "openai/gpt-4") */
  modelId: string;
  /** Optional display label (defaults to model name if not set) */
  label?: string;
  /** Parameters specific to this instance (overrides per-model settings) */
  parameters?: ModelParameters;
}

/**
 * Creates a default ModelInstance from a model ID.
 * Used for backwards compatibility when converting from string[] to ModelInstance[].
 */
export function createDefaultInstance(modelId: string): ModelInstance {
  return {
    id: modelId, // Use modelId as id for simple cases
    modelId,
  };
}

/**
 * Creates a unique instance ID for a new instance of a model.
 * Appends a counter suffix if needed (e.g., "openai/gpt-4-2").
 */
export function createInstanceId(modelId: string, existingInstances: ModelInstance[]): string {
  const existingIds = new Set(existingInstances.map((i) => i.id));

  // If no collision, use modelId directly
  if (!existingIds.has(modelId)) {
    return modelId;
  }

  // Find the next available suffix
  let counter = 2;
  while (existingIds.has(`${modelId}-${counter}`)) {
    counter++;
  }
  return `${modelId}-${counter}`;
}

/**
 * Gets a display label for an instance.
 * Returns the custom label if set, otherwise the model ID.
 */
export function getInstanceLabel(instance: ModelInstance): string {
  return instance.label ?? instance.modelId;
}

/** @deprecated Use ModelParameters directly - systemPrompt is now included */
export type ModelSettings = ModelParameters;

export interface PerModelSettings {
  [modelId: string]: ModelParameters;
}

/** Configuration for which response action buttons to show */
export interface ResponseActionConfig {
  showSelectBest?: boolean;
  showRegenerate?: boolean;
  showCopy?: boolean;
  showExpand?: boolean;
  /** Show hide button for individual response visibility control */
  showHide?: boolean;
  /** Show speak button for TTS playback */
  showSpeak?: boolean;
}

export const DEFAULT_ACTION_CONFIG: ResponseActionConfig = {
  showSelectBest: true,
  showRegenerate: true,
  showCopy: true,
  showExpand: true,
  showHide: true,
  showSpeak: true,
};

export interface ChatState {
  conversations: Conversation[];
  currentConversation: Conversation | null;
  selectedModels: string[];
  isStreaming: boolean;
  streamingResponses: Map<string, string>;
}

// ============================================================================
// Debug Types - For viewing raw message exchanges
// ============================================================================

/** Raw SSE event captured during streaming */
export interface DebugSSEEvent {
  /** Timestamp when event was received */
  timestamp: number;
  /** Event type (e.g., "response.content_part.delta", "response.tool_call.done") */
  type: string;
  /** Raw JSON data of the event */
  data: unknown;
}

/** A single round of the multi-turn tool execution loop */
export interface DebugRound {
  /** Round number (1-indexed) */
  round: number;
  /** Timestamp when this round started */
  startTime: number;
  /** Timestamp when this round ended */
  endTime?: number;

  // === Logical View Data ===

  /** Input items sent to the API for this round */
  inputItems: unknown[];
  /** Full request body sent to /api/v1/responses */
  requestBody?: Record<string, unknown>;
  /** The response.output array from the completed response */
  responseOutput?: unknown[];
  /** Tool calls detected in this round */
  toolCalls?: Array<{
    id: string;
    name: string;
    arguments: unknown;
  }>;
  /** Tool execution results for this round */
  toolResults?: Array<{
    callId: string;
    toolName: string;
    success: boolean;
    output?: string;
    error?: string;
  }>;
  /** Function call output items sent back to continue (for next round) */
  continuationItems?: unknown[];

  // === Raw View Data ===

  /** Raw SSE events received during this round's streaming */
  sseEvents?: DebugSSEEvent[];
}

/** Debug info for a complete message exchange (may have multiple rounds) */
export interface MessageDebugInfo {
  /** ID of the message this debug info is for */
  messageId: string;
  /** Model that generated this response */
  model: string;
  /** All rounds of the tool execution loop */
  rounds: DebugRound[];
  /** Total duration of all rounds */
  totalDuration: number;
  /** Whether the response completed successfully */
  success: boolean;
  /** Error message if the response failed */
  error?: string;
}
