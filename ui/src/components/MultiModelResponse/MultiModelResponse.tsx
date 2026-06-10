import {
  AlertCircle,
  Bot,
  Bug,
  ChevronLeft,
  ChevronRight,
  Columns2,
  Eye,
  EyeOff,
  GitFork,
  MessageSquare,
  MessageSquarePlus,
  Loader2,
  MoreHorizontal,
  Pencil,
  Rows3,
  Square,
  Trophy,
  Volume2,
  X,
} from "lucide-react";
import {
  useMemo,
  memo,
  useCallback,
  useState,
  useRef,
  useLayoutEffect,
  useEffect,
  type MouseEvent,
  type KeyboardEvent,
} from "react";

import { ArtifactList, ArtifactModal } from "@/components/Artifact";
import { CitationList } from "@/components/CitationList";
import { DebugModal } from "@/components/DebugModal";
import { QuoteSelectionPopover } from "@/components/QuoteSelectionPopover";
import type { Artifact as ArtifactType, DisplaySelectionData } from "@/components/chat-types";
import { useDebugInfo } from "@/stores/debugStore";
import { ContentRound } from "./ContentRound";

import { Avatar, AvatarFallback } from "@/components/Avatar/Avatar";
import { Button } from "@/components/Button/Button";
import type {
  Artifact,
  CompletedRound,
  HistoryMode,
  MessageModeMetadata,
  MessageUsage,
  ResponseFeedbackData,
  ResponseActionConfig,
  Citation,
  ToolExecutionRound,
} from "@/components/chat-types";
import { DEFAULT_ACTION_CONFIG } from "@/components/chat-types";
import {
  ResponseActions,
  type ResponseActionConfig as ActionConfig,
} from "@/components/ResponseActions/ResponseActions";
import { Tooltip, TooltipContent, TooltipTrigger } from "@/components/Tooltip/Tooltip";
import { UsageDisplay } from "@/components/UsageDisplay/UsageDisplay";
import {
  Dropdown,
  DropdownTrigger,
  DropdownContent,
  DropdownItem,
  DropdownSeparator,
} from "@/components/Dropdown/Dropdown";
import { Textarea } from "@/components/Textarea/Textarea";
import {
  useViewMode,
  useExpandedModel,
  useChatUIStore,
  useIsEditing,
  useCompactMode,
} from "@/stores/chatUIStore";
import type { PlaybackState } from "@/hooks/useAudioPlayback";
import { useTTSForResponse } from "@/hooks/useTTSManager";
import {
  useCitations,
  useArtifacts,
  useToolExecutionRounds,
  useIsStreaming,
  useHasActiveToolCalls,
  useHasRunningExecution,
  useRunningExecutionStatusMessage,
} from "@/stores/streamingStore";
import { cn } from "@/utils/cn";
import { getModelDisplayName } from "@/utils/modelNames";
import { getModelStyle } from "@/utils/providers";

/**
 * MultiModelResponse - Renders Multiple Model Responses with Layout Options
 *
 * ## Architecture Overview
 *
 * This component renders one or more model responses for a single user message.
 * It supports two layout modes (grid/stacked) and handles both completed and
 * streaming responses.
 *
 * ## Performance-Critical Design
 *
 * This component uses multiple memoization strategies:
 *
 * 1. **Custom arePropsEqual comparator** (see `areMultiModelResponsePropsEqual` at bottom)
 *    - Checks primitive props first (fast)
 *    - Checks callback identity (ensures stable refs from parent)
 *    - Iterates responses array last (most expensive)
 *
 * 2. **Inner ModelResponseCard memo**
 *    - Prevents sibling cards from re-rendering when only one model updates
 *    - During streaming, only the streaming card re-renders
 *
 * 3. **Stable callbacks via useCallback**
 *    - handleExpand, handleSelectBest, handleRegenerate
 *    - Tight dependency arrays prevent unnecessary recreation
 *
 * ## Re-render Behavior
 *
 * **During streaming (token update for "claude-opus"):**
 * ```
 * MultiModelResponse (outer)  ❌ NO RE-RENDER (memo comparator - content diff)
 * └── ModelResponseCard (gpt-4)      ❌ NO RE-RENDER (inner memo)
 * └── ModelResponseCard (claude-opus) ✅ RE-RENDERS
 *     └── StreamingMarkdown           ✅ RE-RENDERS (content changed)
 * ```
 *
 * **Note:** The outer MultiModelResponse may re-render on response content changes,
 * but the custom comparator ensures this only happens when actual data changes,
 * not on every parent render.
 *
 * ## Responsive Behavior
 *
 * Uses ResizeObserver in ModelResponseCard to collapse action buttons into a
 * dropdown menu when card width < 400px. This prevents layout thrashing and
 * keeps the UI usable on narrow viewports.
 */

interface ModelResponse {
  /** Model ID (e.g., "openai/gpt-4") used for API calls and styling */
  model: string;
  /**
   * Instance ID uniquely identifying this response.
   * For multi-instance scenarios (same model, different settings),
   * this distinguishes between instances (e.g., "gpt-4-creative" vs "gpt-4-precise").
   * Falls back to model ID if not set.
   */
  instanceId?: string;
  /**
   * Message ID for the assistant response.
   * Used for edit operations to identify the specific message to update.
   */
  messageId?: string;
  /**
   * Optional display label for the instance.
   * When set, shown alongside or instead of the model name.
   */
  label?: string;
  content: string;
  /** Reasoning content for current/last round (extended thinking) */
  reasoningContent?: string;
  /** Completed rounds bundling reasoning, content, and tool execution (multi-round tool execution) */
  completedRounds?: CompletedRound[];
  isStreaming: boolean;
  error?: string;
  usage?: MessageUsage;
  feedback?: ResponseFeedbackData;
  modeMetadata?: MessageModeMetadata;
  /** Citations from file_search or web_search tools */
  citations?: Citation[];
  /** Artifacts produced by tool execution (charts, tables, images, etc.) */
  artifacts?: Artifact[];
  /** Tool execution timeline for progressive disclosure UI */
  toolExecutionRounds?: ToolExecutionRound[];
  /** Debug message ID for looking up debug info */
  debugMessageId?: string;
}

/** Minimal info needed to display hidden response indicators */
interface HiddenResponse {
  /** Model ID */
  model: string;
  /** Instance ID (for identification) */
  instanceId: string;
  /** Display label if set */
  label?: string;
}

interface MultiModelResponseProps {
  responses: ModelResponse[];
  timestamp: Date;
  /** Optional group ID for identifying which message group this belongs to */
  groupId?: string;
  /** Callback when user selects a response as the best. Uses instanceId to identify the response. */
  onSelectBest?: (groupId: string, instanceId: string | null) => void;
  /** Callback to regenerate a response. Uses instanceId to identify which response to regenerate. */
  onRegenerate?: (groupId: string, instanceId: string) => void;
  /** Callback to hide a response. Uses groupId and instanceId to identify which response to hide. */
  onHide?: (groupId: string, instanceId: string) => void;
  /** Callback to save edited response content and re-run from that point. */
  onSaveEdit?: (messageId: string, newContent: string) => void;
  /** Hidden responses that can be restored */
  hiddenResponses?: HiddenResponse[];
  /** Callback to show a hidden response. Uses groupId and instanceId to identify which response to show. */
  onShowHidden?: (groupId: string, instanceId: string) => void;
  /** The currently selected "best" response instance ID */
  selectedBest?: string | null;
  /** Configuration for which action buttons to show */
  actionConfig?: ResponseActionConfig;
  /** History mode used when this message was sent (read-only display) */
  historyMode?: HistoryMode;
  /** Force stacked layout regardless of global viewMode (used for screenshot export) */
  forceStacked?: boolean;
}

/**
 * Streaming phase for a model response.
 * - "idle": not streaming or content is actively flowing
 * - "thinking": waiting for network, model reasoning, or content stalled
 * - "processing": tool calls are executing
 */
type StreamingPhase = "idle" | "thinking" | "processing";

/** Detect when streaming content has stalled (no new tokens for a threshold period). */
function useContentStalled(content: string, isStreaming: boolean, thresholdMs = 1500): boolean {
  const [stalled, setStalled] = useState(false);

  useEffect(() => {
    if (!isStreaming || !content) {
      setStalled(false);
      return;
    }
    setStalled(false);
    const timer = setTimeout(() => setStalled(true), thresholdMs);
    return () => clearTimeout(timer);
  }, [content, isStreaming, thresholdMs]);

  return stalled;
}

/**
 * Compute the streaming phase for a model response. Centralises all status
 * logic so the rendering layer has a single value to check.
 */
function useStreamingPhase(
  response: {
    isStreaming: boolean;
    content: string;
    reasoningContent?: string;
    completedRounds?: CompletedRound[];
  },
  hasActiveTools: boolean,
  compactMode: boolean
): StreamingPhase {
  const isStalled = useContentStalled(response.content, response.isStreaming);

  if (!response.isStreaming) return "idle";

  const hasContent = !!response.content?.trim();
  const hasReasoning = !!response.reasoningContent;
  const rounds = response.completedRounds;
  const hasRounds = !!rounds?.length;

  // No output at all yet — waiting for first token (unless tools are executing)
  if (!hasContent && !hasReasoning && !hasRounds) {
    return hasActiveTools ? "processing" : "thinking";
  }

  // Multi-round: check whether the current (in-flight) round has content yet
  if (hasRounds) {
    const currentReasoning =
      hasReasoning && !rounds!.some((r) => r.reasoning === response.reasoningContent)
        ? response.reasoningContent
        : null;
    const currentContent = hasContent ? response.content : null;

    // Between rounds — no new content flowing
    if (!currentReasoning && !currentContent) {
      return hasActiveTools ? "processing" : "thinking";
    }

    // Current round content stalled
    if (currentContent && isStalled) {
      return hasActiveTools ? "processing" : "thinking";
    }

    // Compact mode hides reasoning — show thinking when only reasoning is flowing
    if (compactMode && currentReasoning && !currentContent) {
      return "thinking";
    }

    return "idle";
  }

  // Single-round: reasoning streaming but no content yet
  // (non-compact shows ReasoningSection which has its own indicator)
  if (hasReasoning && !hasContent) {
    return compactMode ? "thinking" : "idle";
  }

  // Content stalled
  if (isStalled) {
    return hasActiveTools ? "processing" : "thinking";
  }

  return "idle";
}

const PHASE_LABEL: Record<StreamingPhase, string> = {
  idle: "",
  thinking: "Thinking",
  processing: "Processing",
};

/** Animated dots + label shown when the model is thinking or processing. */
function StreamingStatusIndicator({
  phase,
  toolStatusMessage,
}: {
  phase: StreamingPhase;
  toolStatusMessage?: string;
}) {
  if (phase === "idle") return null;
  // Strip trailing dots/ellipsis from tool status messages since we show animated dots
  const label =
    (phase === "processing" && toolStatusMessage?.replace(/\.+$/, "")) || PHASE_LABEL[phase];
  return (
    <div className="flex items-baseline gap-1.5 pl-3 border-l-2 border-transparent text-muted-foreground">
      <span className="text-sm">{label}</span>
      <div className="flex items-baseline gap-1 py-1">
        <span className="h-1 w-1 rounded-full bg-muted-foreground/60 animate-typing-dot" />
        <span className="h-1 w-1 rounded-full bg-muted-foreground/60 animate-typing-dot-delay-1" />
        <span className="h-1 w-1 rounded-full bg-muted-foreground/60 animate-typing-dot-delay-2" />
      </div>
    </div>
  );
}

interface ModelResponseCardProps {
  response: ModelResponse;
  model: string;
  /**
   * Message group ID for identifying this response in the conversation.
   * Used together with instanceId for TTS tracking.
   */
  groupId: string;
  /**
   * Instance ID uniquely identifying this response.
   * Used for callbacks and React keys in multi-instance scenarios.
   */
  instanceId: string;
  /**
   * Message ID for the assistant response.
   * Used for edit operations to identify the specific message to update.
   */
  messageId?: string;
  /**
   * Optional display label for the instance.
   * When set, shown in the header alongside the model name.
   */
  label?: string;
  index: number;
  isExpanded?: boolean;
  onExpand?: (instanceId: string) => void;
  onSelectBest?: (instanceId: string) => void;
  onRegenerate?: (instanceId: string) => void;
  onHide?: (instanceId: string) => void;
  /** Callback to save edited content. For assistant messages, just updates content. */
  onSaveEdit?: (instanceId: string, newContent: string) => void;
  isSelectedBest?: boolean;
  showSelectBest?: boolean;
  actionConfig: ActionConfig;
  /** Whether to use horizontal layout with fixed-width cards */
  useHorizontalLayout?: boolean;
}

/** Collapsed menu for actions when space is constrained */
interface CollapsedActionsMenuProps {
  content: string;
  usage?: MessageUsage;
  isSelectedBest?: boolean;
  showSelectBest?: boolean;
  isExpanded?: boolean;
  canExpand?: boolean;
  onSelectBest?: () => void;
  onRegenerate?: () => void;
  onExpand?: () => void;
  onHide?: () => void;
  onSpeak?: () => void;
  onStopSpeaking?: () => void;
  speakingState?: PlaybackState;
  onEdit?: () => void;
  onOpenDebug?: () => void;
  hasDebugInfo?: boolean;
  actionConfig: ActionConfig;
}

function CollapsedActionsMenu({
  content,
  usage,
  isSelectedBest,
  showSelectBest,
  isExpanded,
  canExpand,
  onSelectBest,
  onRegenerate,
  onExpand,
  onHide,
  onSpeak,
  onStopSpeaking,
  speakingState = "idle",
  onEdit,
  onOpenDebug,
  hasDebugInfo,
  actionConfig,
}: CollapsedActionsMenuProps) {
  const [copied, setCopied] = useState(false);

  const handleCopy = async () => {
    try {
      await navigator.clipboard.writeText(content);
      setCopied(true);
      setTimeout(() => setCopied(false), 2000);
    } catch (err) {
      console.debug("Clipboard write failed", err);
    }
  };

  const isSpeaking = speakingState === "playing";
  const isSpeakLoading = speakingState === "loading";
  const canStopSpeaking =
    (isSpeaking || isSpeakLoading || speakingState === "paused") && onStopSpeaking;

  const showSelectBestAction =
    actionConfig.showSelectBest && showSelectBest && onSelectBest && !isSelectedBest;
  const showRegenerateAction = actionConfig.showRegenerate && onRegenerate;
  const showCopyAction = actionConfig.showCopy;
  const showExpandAction = actionConfig.showExpand && canExpand && onExpand;
  const showHideAction = actionConfig.showHide && onHide;
  const showSpeakAction = actionConfig.showSpeak && onSpeak;
  const showEditAction = !!onEdit;

  return (
    <Dropdown>
      <DropdownTrigger asChild showChevron={false}>
        <Button
          variant="ghost"
          size="sm"
          className="h-7 w-7 p-0 shrink-0"
          aria-label="Response actions"
        >
          <MoreHorizontal className="h-4 w-4" />
        </Button>
      </DropdownTrigger>
      <DropdownContent align="end" className="min-w-[180px]">
        {/* Usage info */}
        {usage && (
          <>
            <div className="px-2.5 py-2 text-xs text-muted-foreground">
              <div className="font-medium mb-1">Token Usage</div>
              <div>Total: {usage.totalTokens.toLocaleString()}</div>
              {usage.cost !== undefined && usage.cost > 0 && (
                <div>Cost: ${usage.cost.toFixed(4)}</div>
              )}
            </div>
            <DropdownSeparator />
          </>
        )}

        {/* Actions */}
        {showCopyAction && (
          <DropdownItem onClick={handleCopy}>{copied ? "Copied!" : "Copy response"}</DropdownItem>
        )}
        {showRegenerateAction && <DropdownItem onClick={onRegenerate}>Regenerate</DropdownItem>}
        {showExpandAction && (
          <DropdownItem onClick={onExpand}>{isExpanded ? "Collapse" : "Expand"}</DropdownItem>
        )}
        {showSelectBestAction && <DropdownItem onClick={onSelectBest}>Select as best</DropdownItem>}
        {showHideAction && (
          <DropdownItem onClick={onHide}>
            <EyeOff className="h-4 w-4 mr-2" />
            Hide response
          </DropdownItem>
        )}
        {showSpeakAction && (
          <DropdownItem
            onClick={canStopSpeaking ? onStopSpeaking : onSpeak}
            disabled={isSpeakLoading}
          >
            {isSpeakLoading ? (
              <Loader2 className="h-4 w-4 mr-2 animate-spin" />
            ) : canStopSpeaking ? (
              <Square className="h-4 w-4 mr-2" />
            ) : (
              <Volume2 className="h-4 w-4 mr-2" />
            )}
            {isSpeakLoading
              ? "Generating audio..."
              : canStopSpeaking
                ? "Stop speaking"
                : "Read aloud"}
          </DropdownItem>
        )}
        {showEditAction && (
          <DropdownItem onClick={onEdit}>
            <Pencil className="h-4 w-4 mr-2" />
            Edit response
          </DropdownItem>
        )}

        {/* Debug */}
        {hasDebugInfo && onOpenDebug && (
          <>
            <DropdownSeparator />
            <DropdownItem onClick={onOpenDebug}>
              <Bug className="h-4 w-4 mr-2" />
              View debug info
            </DropdownItem>
          </>
        )}
      </DropdownContent>
    </Dropdown>
  );
}

// Minimum width to show full controls (usage + actions)
const MIN_WIDTH_FOR_FULL_CONTROLS = 400;

/**
 * ModelResponseCard - Individual Model Response with Memoization
 *
 * This inner component is memoized to prevent re-renders of sibling cards.
 * During multi-model streaming, only the card whose content is changing re-renders.
 *
 * Uses ResizeObserver to adaptively collapse controls when width is constrained.
 */
const ModelResponseCard = memo(function ModelResponseCard({
  response,
  model,
  groupId,
  instanceId,
  messageId,
  label,
  index,
  isExpanded,
  onExpand,
  onSelectBest,
  onRegenerate,
  onHide,
  onSaveEdit,
  isSelectedBest,
  showSelectBest,
  actionConfig,
  useHorizontalLayout,
}: ModelResponseCardProps) {
  const modelDisplayName = getModelDisplayName(model);
  // Show instance label if set and different from the model display name
  const displayName = label && label !== modelDisplayName ? label : modelDisplayName;
  const showInstanceLabel = label && label !== modelDisplayName;
  const style = getModelStyle(model);
  const isComplete =
    !response.isStreaming &&
    (response.content || response.artifacts?.length || response.reasoningContent) &&
    !response.error;
  const isAnyStreaming = useIsStreaming();
  const compactMode = useCompactMode();
  // Only query streaming store for actively-streaming responses — committed
  // messages must not read the current stream's state (same model key would
  // leak status from a newer message into older ones).
  const streamingModelKey = response.isStreaming ? instanceId : "";
  const hasActiveToolCalls = useHasActiveToolCalls(streamingModelKey);
  const hasRunningExecution = useHasRunningExecution(streamingModelKey);
  const hasActiveTools = hasActiveToolCalls || hasRunningExecution;
  const toolStatusMessage = useRunningExecutionStatusMessage(streamingModelKey);
  const streamingPhase = useStreamingPhase(response, hasActiveTools, compactMode);

  // State for artifact modal
  const [selectedArtifact, setSelectedArtifact] = useState<ArtifactType | null>(null);
  const handleArtifactClick = useCallback((artifact: ArtifactType) => {
    setSelectedArtifact(artifact);
  }, []);
  const handleArtifactModalClose = useCallback(() => {
    setSelectedArtifact(null);
  }, []);

  // State for debug modal
  const [showDebugModal, setShowDebugModal] = useState(false);
  const debugInfo = useDebugInfo(response.debugMessageId, model);
  const hasDebugInfo = !!debugInfo;
  const handleOpenDebug = useCallback(() => {
    setShowDebugModal(true);
  }, []);
  const handleCloseDebug = useCallback(() => {
    setShowDebugModal(false);
  }, []);

  // State for quote selection popover
  const [quotePopover, setQuotePopover] = useState<{
    isOpen: boolean;
    position: { x: number; y: number };
    selectedText: string;
  }>({ isOpen: false, position: { x: 0, y: 0 }, selectedText: "" });
  const { setQuotedText } = useChatUIStore();

  const handleContentMouseUp = useCallback(
    (e: MouseEvent) => {
      // Don't show quote popover during streaming
      if (response.isStreaming) return;

      const selection = window.getSelection();
      const selectedText = selection?.toString().trim() || "";

      if (selectedText.length > 0) {
        setQuotePopover({
          isOpen: true,
          position: { x: e.clientX, y: e.clientY },
          selectedText,
        });
      }
    },
    [response.isStreaming]
  );

  const handleQuote = useCallback(
    (text: string) => {
      setQuotedText({
        messageId: groupId,
        instanceId,
        text,
      });
    },
    [setQuotedText, groupId, instanceId]
  );

  const handleCloseQuotePopover = useCallback(() => {
    setQuotePopover((prev) => ({ ...prev, isOpen: false }));
  }, []);

  // Inline editing state - use a namespaced composite key so it can never
  // collide with the `chat:<message.id>` keys ChatMessage writes into the
  // same global slot.
  const editingKey = `multi:${groupId}:${instanceId}`;
  const isEditing = useIsEditing(editingKey);
  const [editContent, setEditContent] = useState(response.content);
  const textareaRef = useRef<HTMLTextAreaElement>(null);
  const responseContentRef = useRef(response.content);
  responseContentRef.current = response.content;
  const { startEditing, stopEditing } = useChatUIStore();

  // Reset edit content when response content changes or editing starts
  useEffect(() => {
    if (isEditing) {
      setEditContent(response.content);
      setTimeout(() => textareaRef.current?.focus(), 0);
    }
  }, [isEditing, response.content]);

  const handleStartEdit = useCallback(() => {
    startEditing(editingKey);
  }, [startEditing, editingKey]);

  const handleCancelEdit = useCallback(() => {
    setEditContent(responseContentRef.current);
    stopEditing();
  }, [stopEditing]);

  const handleSaveEditClick = useCallback(() => {
    if (editContent.trim() && editContent !== responseContentRef.current && messageId) {
      onSaveEdit?.(messageId, editContent.trim());
    }
    stopEditing();
  }, [editContent, messageId, onSaveEdit, stopEditing]);

  const handleEditKeyDown = useCallback(
    (e: KeyboardEvent<HTMLTextAreaElement>) => {
      if (e.key === "Escape") {
        e.preventDefault();
        handleCancelEdit();
      } else if (e.key === "Enter" && (e.metaKey || e.ctrlKey)) {
        e.preventDefault();
        handleSaveEditClick();
      }
    },
    [handleCancelEdit, handleSaveEditClick]
  );

  // TTS playback state and callbacks for this response
  const {
    state: ttsState,
    speak: handleSpeak,
    stop: handleStopSpeaking,
  } = useTTSForResponse(response.content, groupId, instanceId);

  // Get citations from streaming store (for active/recent streams) or from response props.
  // Use streamingModelKey ("" for committed) to avoid leaking state across messages.
  const streamingCitations = useCitations(streamingModelKey);
  const citations = useMemo(() => {
    if (streamingCitations.length > 0) return streamingCitations;
    return response.citations ?? [];
  }, [streamingCitations, response.citations]);
  const hasCitations = citations.length > 0;

  // Get artifacts from streaming store (for active/recent streams) or from response props
  const streamingArtifacts = useArtifacts(streamingModelKey);
  const artifacts = useMemo(() => {
    if (streamingArtifacts.length > 0) return streamingArtifacts;
    return response.artifacts ?? [];
  }, [streamingArtifacts, response.artifacts]);
  const hasArtifacts = artifacts.length > 0;

  // Get tool execution rounds from streaming store (for active/recent streams) or from response props
  const streamingToolExecutionRounds = useToolExecutionRounds(streamingModelKey);
  const toolExecutionRounds = useMemo(() => {
    if (streamingToolExecutionRounds.length > 0) return streamingToolExecutionRounds;
    return response.toolExecutionRounds ?? [];
  }, [streamingToolExecutionRounds, response.toolExecutionRounds]);
  const hasToolExecutionRounds = toolExecutionRounds.length > 0;

  // completedRounds is always populated for streaming responses. For committed messages
  // loaded from the database or provided directly (e.g., stories), synthesize rounds
  // from the response's content/reasoning/toolExecutionRounds so rendering still works.
  const completedRounds = useMemo(() => {
    if (response.completedRounds?.length) return response.completedRounds;
    if (response.isStreaming) return [];
    const hasContent = !!response.content?.trim();
    const hasReasoning = !!response.reasoningContent;
    const toolRounds = response.toolExecutionRounds ?? [];
    if (!hasContent && !hasReasoning && !toolRounds.length) return [];
    if (toolRounds.length <= 1) {
      return [
        {
          reasoning: response.reasoningContent || undefined,
          content: response.content || undefined,
          toolExecution: toolRounds[0],
        },
      ];
    }
    // Multiple tool rounds: each round gets its own entry, content goes on the last
    return toolRounds.map((tr, i) => ({
      reasoning: i === 0 ? response.reasoningContent || undefined : undefined,
      content: i === toolRounds.length - 1 ? response.content || undefined : undefined,
      toolExecution: tr,
    }));
  }, [
    response.completedRounds,
    response.isStreaming,
    response.content,
    response.reasoningContent,
    response.toolExecutionRounds,
  ]);

  // Bridge live tool execution data into the last completed round so that
  // ContentRound can render the execution timeline while tools are still running.
  // Gated on isStreaming so committed (DB-loaded) messages are unaffected.
  const completedRoundsWithLiveTools = useMemo(() => {
    if (!response.isStreaming || !toolExecutionRounds.length) {
      return completedRounds;
    }
    const liveRound = toolExecutionRounds[toolExecutionRounds.length - 1];
    // No completed rounds yet (model called tool immediately) — synthesize one
    if (!completedRounds.length) {
      return [{ toolExecution: liveRound }];
    }
    const last = completedRounds[completedRounds.length - 1];
    // Last round already has tool execution. The local round object attached
    // via setCompletedRoundToolExecution and the store's live round are
    // separate objects, so compare by round number. Same number means the
    // tool finished and the next round's text is streaming via showInFlight —
    // don't duplicate. Only append when liveRound is genuinely new
    // (back-to-back tool calls).
    if (last.toolExecution) {
      if (last.toolExecution.round === liveRound.round) {
        return completedRounds;
      }
      return [...completedRounds, { toolExecution: liveRound }];
    }
    // Last round is text-only — inject live tools into it
    const merged = [...completedRounds];
    merged[merged.length - 1] = {
      ...last,
      toolExecution: liveRound,
    };
    return merged;
  }, [completedRounds, toolExecutionRounds, response.isStreaming]);

  // All output artifacts across all rounds (for resolving display_artifacts selections)
  const allOutputArtifacts = useMemo(() => {
    const result: ArtifactType[] = [];
    for (const round of completedRoundsWithLiveTools) {
      if (round.toolExecution) {
        for (const execution of round.toolExecution.executions) {
          for (const a of execution.outputArtifacts) {
            if (a.type !== "display_selection") result.push(a);
          }
        }
      }
    }
    return result;
  }, [completedRoundsWithLiveTools]);

  // Extract display selection for a specific tool execution round.
  // Merges selections from all executions in the round — both explicit
  // `display_artifacts` calls and inline `display` directives on artifact-producing tools.
  const getDisplaySelectionForRound = useCallback(
    (round: ToolExecutionRound): DisplaySelectionData | null => {
      const mergedIds: string[] = [];
      let layout: DisplaySelectionData["layout"] | null = null;
      for (const execution of round.executions) {
        for (const artifact of execution.outputArtifacts) {
          if (artifact.type !== "display_selection") continue;
          const data = artifact.data as DisplaySelectionData;
          mergedIds.push(...data.artifactIds);
          if (!layout) layout = data.layout;
        }
      }
      if (mergedIds.length === 0) return null;
      return { artifactIds: [...new Set(mergedIds)], layout: layout ?? "inline" };
    },
    []
  );

  // Measure header width to determine if we should collapse controls
  const headerRef = useRef<HTMLDivElement>(null);
  const [isCollapsed, setIsCollapsed] = useState(false);

  useLayoutEffect(() => {
    if (!headerRef.current) return;

    const observer = new ResizeObserver((entries) => {
      for (const entry of entries) {
        setIsCollapsed(entry.contentRect.width < MIN_WIDTH_FOR_FULL_CONTROLS);
      }
    });

    observer.observe(headerRef.current);
    return () => observer.disconnect();
  }, []);

  // Create stable callbacks that bind the instanceId
  const handleExpand = useCallback(() => onExpand?.(instanceId), [onExpand, instanceId]);
  const handleSelectBest = useCallback(
    () => onSelectBest?.(instanceId),
    [onSelectBest, instanceId]
  );
  const handleRegenerate = useCallback(
    () => onRegenerate?.(instanceId),
    [onRegenerate, instanceId]
  );
  const handleHide = useCallback(() => onHide?.(instanceId), [onHide, instanceId]);

  return (
    <div
      className={cn(
        "flex flex-col rounded-xl border shadow-sm transition-all duration-300",
        "hover:shadow-md",
        "animate-slide-up-bounce",
        isSelectedBest && "ring-2 ring-success ring-offset-2 ring-offset-background",
        useHorizontalLayout ? "grow shrink-0 basis-[min(500px,85vw)]" : "w-full"
      )}
      style={{ animationDelay: `${index * 100}ms` }}
    >
      {/* Header */}
      <div
        ref={headerRef}
        className="group/card flex items-center gap-2 border-b px-3 py-2.5 min-w-0"
      >
        {/* Left side: Avatar and model name */}
        <div className="flex items-center gap-2 min-w-0 flex-1">
          <Avatar className="h-7 w-7 shrink-0">
            <AvatarFallback className={cn("border", style.bgColor, style.borderColor)}>
              <Bot className={cn("h-3.5 w-3.5", style.color)} />
            </AvatarFallback>
          </Avatar>
          <Tooltip>
            <TooltipTrigger asChild>
              <span
                className={cn(
                  "inline-flex items-center rounded-md border px-2 py-0.5 text-xs font-semibold truncate",
                  style.bgColor,
                  style.borderColor,
                  style.color
                )}
              >
                {displayName}
              </span>
            </TooltipTrigger>
            <TooltipContent side="bottom" className="text-xs">
              <div className="space-y-1">
                {showInstanceLabel && <div className="font-medium text-primary">{label}</div>}
                <div className="font-medium">{modelDisplayName}</div>
                <div className="text-muted-foreground font-mono text-[10px]">{response.model}</div>
              </div>
            </TooltipContent>
          </Tooltip>
          {isSelectedBest && (
            <Tooltip>
              <TooltipTrigger asChild>
                <button
                  onClick={handleSelectBest}
                  className="inline-flex items-center gap-1 rounded-full bg-success/10 px-1.5 py-0.5 text-[10px] font-medium text-success hover:bg-success/20 transition-colors shrink-0"
                  aria-label="Deselect best response"
                >
                  <Trophy className="h-2.5 w-2.5" />
                  Best
                </button>
              </TooltipTrigger>
              <TooltipContent>Click to deselect</TooltipContent>
            </Tooltip>
          )}
        </div>

        {/* Right side: Usage and actions - collapsed or expanded based on available width */}
        <div className="flex items-center gap-2 shrink-0">
          {isComplete && isCollapsed ? (
            <CollapsedActionsMenu
              content={response.content}
              usage={response.usage}
              isSelectedBest={isSelectedBest}
              showSelectBest={showSelectBest}
              isExpanded={isExpanded}
              canExpand={!!onExpand}
              onSelectBest={onSelectBest ? handleSelectBest : undefined}
              onRegenerate={onRegenerate ? handleRegenerate : undefined}
              onExpand={onExpand ? handleExpand : undefined}
              onHide={onHide ? handleHide : undefined}
              onSpeak={handleSpeak}
              onStopSpeaking={handleStopSpeaking}
              speakingState={ttsState}
              onEdit={
                onSaveEdit && messageId && !isEditing && !isAnyStreaming
                  ? handleStartEdit
                  : undefined
              }
              onOpenDebug={hasDebugInfo ? handleOpenDebug : undefined}
              hasDebugInfo={hasDebugInfo}
              actionConfig={actionConfig}
            />
          ) : (
            <>
              {/* Usage shows mid-stream too: server-tool loops report
                  cumulative tokens/cost at each turn boundary. */}
              {(isComplete || response.isStreaming) && response.usage && (
                <UsageDisplay usage={response.usage} provisional={!isComplete} />
              )}
              {isComplete && (
                <ResponseActions
                  content={response.content}
                  isSelectedBest={isSelectedBest}
                  canSelectBest={showSelectBest}
                  isExpanded={isExpanded}
                  canExpand={!!onExpand}
                  onSelectBest={onSelectBest ? handleSelectBest : undefined}
                  onRegenerate={onRegenerate ? handleRegenerate : undefined}
                  onExpand={onExpand ? handleExpand : undefined}
                  onHide={onHide ? handleHide : undefined}
                  onSpeak={handleSpeak}
                  onStopSpeaking={handleStopSpeaking}
                  speakingState={ttsState}
                  onEdit={
                    onSaveEdit && messageId && !isEditing && !isAnyStreaming
                      ? handleStartEdit
                      : undefined
                  }
                  onOpenDebug={hasDebugInfo ? handleOpenDebug : undefined}
                  config={actionConfig}
                />
              )}
            </>
          )}
        </div>
      </div>

      {/* Streaming status announcement for screen readers. Per-token
          updates would flood; a hidden status region announces
          start/finish only. */}
      <div role="status" aria-live="polite" className="sr-only">
        {response.isStreaming ? `${response.model ?? "Model"} is responding` : ""}
      </div>

      {/* Content */}
      {/* eslint-disable-next-line jsx-a11y/no-static-element-interactions, jsx-a11y/no-noninteractive-tabindex -- onMouseUp for text selection quoting; tabIndex for scrollable region keyboard access (axe: scrollable-region-focusable) */}
      <div className="flex-1 p-4 overflow-auto" tabIndex={0} onMouseUp={handleContentMouseUp}>
        {response.error ? (
          <div className="flex items-start gap-3 rounded-lg bg-destructive/10 px-4 py-3 text-destructive">
            <AlertCircle className="h-5 w-5 shrink-0 mt-0.5" />
            <span className="text-sm leading-relaxed">{response.error}</span>
          </div>
        ) : isEditing ? (
          <div className="flex flex-col gap-3">
            <Textarea
              ref={textareaRef}
              value={editContent}
              onChange={(e) => setEditContent(e.target.value)}
              onKeyDown={handleEditKeyDown}
              className="min-h-[200px] resize-y font-mono text-sm"
              placeholder="Edit response..."
            />
            <div className="flex items-center justify-between">
              <span className="text-xs text-muted-foreground">
                Ctrl+Enter to save · Escape to cancel
              </span>
              <div className="flex gap-2">
                <Button variant="ghost" size="sm" onClick={handleCancelEdit}>
                  <X className="h-3 w-3 mr-1" />
                  Cancel
                </Button>
                <Button
                  variant="primary"
                  size="sm"
                  onClick={handleSaveEditClick}
                  disabled={!editContent.trim() || editContent === response.content}
                >
                  Save
                </Button>
              </div>
            </div>
          </div>
        ) : (
          <>
            {/* Content: unified rendering via ContentRound for all responses */}
            {(() => {
              // Detect in-flight content that hasn't been captured in a completed round yet.
              // completedRoundsWithLiveTools is always populated (even for single-round responses),
              // so this only shows content actively streaming in the current round.
              const currentReasoning =
                response.isStreaming &&
                response.reasoningContent &&
                !completedRoundsWithLiveTools.some((r) => r.reasoning === response.reasoningContent)
                  ? response.reasoningContent
                  : null;
              const currentContent =
                response.isStreaming && response.content?.trim() ? response.content : null;
              const showInFlight = currentReasoning || currentContent;

              // Suppress the streaming status indicator when running tools are
              // already visible in expanded ContentRounds (non-compact mode only)
              const hasVisibleRunningTools =
                !compactMode &&
                completedRoundsWithLiveTools.some((r) =>
                  r.toolExecution?.executions.some(
                    (e) => e.status === "running" || e.status === "pending"
                  )
                );

              return (
                <div className="space-y-3">
                  {completedRoundsWithLiveTools.map((round, i) => (
                    <ContentRound
                      key={i}
                      reasoning={round.reasoning}
                      content={round.content}
                      reasoningTokenCount={
                        completedRoundsWithLiveTools.length === 1
                          ? response.usage?.reasoningTokens
                          : undefined
                      }
                      toolExecutionRound={round.toolExecution}
                      isToolsStreaming={
                        response.isStreaming &&
                        i === completedRoundsWithLiveTools.length - 1 &&
                        !!round.toolExecution?.executions.some(
                          (e) => e.status === "pending" || e.status === "running"
                        )
                      }
                      onArtifactClick={handleArtifactClick}
                      displaySelection={
                        round.toolExecution
                          ? getDisplaySelectionForRound(round.toolExecution)
                          : null
                      }
                      allOutputArtifacts={allOutputArtifacts}
                    />
                  ))}
                  {showInFlight && (
                    <ContentRound
                      reasoning={currentReasoning}
                      content={currentContent}
                      isStreaming={response.isStreaming}
                      isReasoningStreaming={response.isStreaming && !currentContent}
                    />
                  )}
                  {!hasVisibleRunningTools && (
                    <StreamingStatusIndicator
                      phase={streamingPhase}
                      toolStatusMessage={toolStatusMessage}
                    />
                  )}
                </div>
              );
            })()}
            {/* Citations from file_search/web_search */}
            {hasCitations && (
              <CitationList citations={citations} className="mt-4 pt-4 border-t" compact={false} />
            )}
            {/* Standalone artifacts (not from tool execution) */}
            {hasArtifacts && !hasToolExecutionRounds && (
              <ArtifactList artifacts={artifacts} className="mt-4 pt-4 border-t" />
            )}
          </>
        )}
      </div>

      {/* Artifact expansion modal */}
      <ArtifactModal
        artifact={selectedArtifact}
        open={selectedArtifact !== null}
        onClose={handleArtifactModalClose}
      />

      {/* Debug modal */}
      {showDebugModal && debugInfo && (
        <DebugModal debugInfo={debugInfo} onClose={handleCloseDebug} />
      )}

      {/* Quote selection popover */}
      <QuoteSelectionPopover
        isOpen={quotePopover.isOpen}
        position={quotePopover.position}
        selectedText={quotePopover.selectedText}
        onQuote={handleQuote}
        onClose={handleCloseQuotePopover}
      />
    </div>
  );
});

function MultiModelResponseComponent({
  responses,
  timestamp,
  groupId,
  onSelectBest,
  onRegenerate,
  onHide,
  onSaveEdit,
  hiddenResponses = [],
  onShowHidden,
  selectedBest,
  actionConfig = DEFAULT_ACTION_CONFIG,
  historyMode,
  forceStacked = false,
}: MultiModelResponseProps) {
  // Use global UI state from store
  const viewMode = useViewMode();
  const expandedModel = useExpandedModel();
  const compactMode = useCompactMode();
  const { setViewMode, setExpandedModel, toggleCompactMode } = useChatUIStore();

  const isMultiResponse = responses.length > 1;
  const showViewToggle = isMultiResponse;
  const showSelectBest = isMultiResponse && !responses.some((r) => r.isStreaming);

  // Helper to get the effective instance ID (falls back to model for backwards compat)
  const getInstanceId = (r: ModelResponse) => r.instanceId ?? r.model;

  // Sort responses: selected best first, then original order
  const sortedResponses = useMemo(() => {
    return [...responses].sort((a, b) => {
      // Selected best always first (compare by instanceId)
      const aInstanceId = getInstanceId(a);
      const bInstanceId = getInstanceId(b);
      if (aInstanceId === selectedBest) return -1;
      if (bInstanceId === selectedBest) return 1;
      // Keep original order for the rest
      return 0;
    });
  }, [responses, selectedBest]);

  // If one is expanded, only show that one (compare by instanceId)
  const displayedResponses = expandedModel
    ? sortedResponses.filter((r) => getInstanceId(r) === expandedModel)
    : sortedResponses;

  // Convert action config to the ResponseActions format
  const actionsConfig: ActionConfig = useMemo(
    () => ({
      showSelectBest: actionConfig.showSelectBest,
      showRegenerate: actionConfig.showRegenerate,
      showCopy: actionConfig.showCopy,
      showExpand: actionConfig.showExpand,
      showHide: actionConfig.showHide,
      showSpeak: actionConfig.showSpeak,
    }),
    [actionConfig]
  );

  // Stable callback creators for ModelResponseCard (use instanceId for identification)
  const handleExpand = useCallback(
    (instanceId: string) => {
      // Toggle: if already expanded, collapse; otherwise expand this instance
      setExpandedModel(expandedModel === instanceId ? null : instanceId);
    },
    [setExpandedModel, expandedModel]
  );

  const handleSelectBest = useCallback(
    (instanceId: string) => {
      if (groupId) {
        if (selectedBest === instanceId) {
          onSelectBest?.(groupId, null);
        } else {
          onSelectBest?.(groupId, instanceId);
        }
      }
    },
    [onSelectBest, selectedBest, groupId]
  );

  const handleRegenerate = useCallback(
    (instanceId: string) => {
      if (groupId) {
        onRegenerate?.(groupId, instanceId);
      }
    },
    [onRegenerate, groupId]
  );

  const handleHide = useCallback(
    (instanceId: string) => {
      if (groupId) {
        onHide?.(groupId, instanceId);
      }
    },
    [onHide, groupId]
  );

  const handleSaveEdit = useCallback(
    (messageId: string, newContent: string) => {
      onSaveEdit?.(messageId, newContent);
    },
    [onSaveEdit]
  );

  const handleShowHidden = useCallback(
    (instanceId: string) => {
      if (groupId) {
        onShowHidden?.(groupId, instanceId);
      }
    },
    [onShowHidden, groupId]
  );

  const handleShowAllHidden = useCallback(() => {
    if (groupId) {
      hiddenResponses.forEach((r) => {
        onShowHidden?.(groupId, r.instanceId);
      });
    }
  }, [onShowHidden, groupId, hiddenResponses]);

  const hasHiddenResponses = hiddenResponses.length > 0;

  // "grid" = horizontal layout with fixed-width cards, "stacked" = vertical full-width
  const useHorizontalLayout = !forceStacked && viewMode === "grid" && displayedResponses.length > 1;

  // Horizontal scroll navigation state
  const scrollContainerRef = useRef<HTMLDivElement>(null);
  const [canScrollLeft, setCanScrollLeft] = useState(false);
  const [canScrollRight, setCanScrollRight] = useState(false);

  const updateScrollState = useCallback(() => {
    const el = scrollContainerRef.current;
    if (!el) return;
    setCanScrollLeft(el.scrollLeft > 0);
    setCanScrollRight(el.scrollLeft + el.clientWidth < el.scrollWidth - 1);
  }, []);

  useEffect(() => {
    if (!useHorizontalLayout) return;
    const el = scrollContainerRef.current;
    if (!el) return;

    updateScrollState();
    el.addEventListener("scroll", updateScrollState, { passive: true });
    const observer = new ResizeObserver(updateScrollState);
    observer.observe(el);

    return () => {
      el.removeEventListener("scroll", updateScrollState);
      observer.disconnect();
    };
  }, [useHorizontalLayout, updateScrollState]);

  const SCROLL_AMOUNT = 512; // 500px card + 12px gap

  const handleScrollBy = useCallback((direction: "left" | "right") => {
    scrollContainerRef.current?.scrollBy({
      left: direction === "left" ? -SCROLL_AMOUNT : SCROLL_AMOUNT,
      behavior: "smooth",
    });
  }, []);

  const handleScrollKeyDown = useCallback(
    (e: KeyboardEvent<HTMLDivElement>) => {
      if (e.key === "ArrowLeft") {
        e.preventDefault();
        handleScrollBy("left");
      } else if (e.key === "ArrowRight") {
        e.preventDefault();
        handleScrollBy("right");
      }
    },
    [handleScrollBy]
  );

  const layoutClasses = cn(
    "gap-3 p-1", // p-1 provides space for ring-offset on selected best cards
    useHorizontalLayout ? "flex items-start overflow-x-auto pb-2 scrollbar-thin" : "flex flex-col"
  );

  return (
    <div className="py-4">
      {/* Header */}
      <div className="mb-3 flex items-center gap-2">
        <div className="h-px flex-1 bg-border" />
        <span className="text-xs text-muted-foreground">
          {timestamp.toLocaleTimeString([], {
            hour: "2-digit",
            minute: "2-digit",
          })}
        </span>
        {isMultiResponse && (
          <span className="rounded-full bg-muted px-2 py-0.5 text-xs text-muted-foreground">
            {responses.length} responses
          </span>
        )}
        {/* History mode badge - only show when there are multiple models and historyMode is set */}
        {isMultiResponse && historyMode && historyMode !== "all" && (
          <Tooltip>
            <TooltipTrigger asChild>
              <span
                className={cn(
                  "inline-flex items-center gap-1.5 rounded-full px-2.5 py-1 text-xs",
                  "bg-primary/10 text-primary"
                )}
              >
                <GitFork className="h-3 w-3" />
                Same model
              </span>
            </TooltipTrigger>
            <TooltipContent>Each model only saw its own previous responses</TooltipContent>
          </Tooltip>
        )}
        {showViewToggle && !expandedModel && (
          <div className="flex items-center gap-0.5 rounded-md border bg-muted/50 p-0.5">
            <Tooltip>
              <TooltipTrigger asChild>
                <Button
                  variant={viewMode === "grid" ? "secondary" : "ghost"}
                  size="sm"
                  className="h-6 w-6 p-0"
                  onClick={() => setViewMode("grid")}
                  aria-label="View side by side"
                >
                  <Columns2 className="h-3.5 w-3.5" />
                </Button>
              </TooltipTrigger>
              <TooltipContent>Side by side</TooltipContent>
            </Tooltip>
            <Tooltip>
              <TooltipTrigger asChild>
                <Button
                  variant={viewMode === "stacked" ? "secondary" : "ghost"}
                  size="sm"
                  className="h-6 w-6 p-0"
                  onClick={() => setViewMode("stacked")}
                  aria-label="View stacked"
                >
                  <Rows3 className="h-3.5 w-3.5" />
                </Button>
              </TooltipTrigger>
              <TooltipContent>Stacked</TooltipContent>
            </Tooltip>
          </div>
        )}
        <div className="flex items-center gap-0.5 rounded-md border bg-muted/50 p-0.5">
          <Tooltip>
            <TooltipTrigger asChild>
              <Button
                variant={compactMode ? "secondary" : "ghost"}
                size="sm"
                className="h-6 w-6 p-0"
                onClick={() => !compactMode && toggleCompactMode()}
                aria-label="Compact view"
              >
                <MessageSquare className="h-3.5 w-3.5" />
              </Button>
            </TooltipTrigger>
            <TooltipContent>Compact view</TooltipContent>
          </Tooltip>
          <Tooltip>
            <TooltipTrigger asChild>
              <Button
                variant={!compactMode ? "secondary" : "ghost"}
                size="sm"
                className="h-6 w-6 p-0"
                onClick={() => compactMode && toggleCompactMode()}
                aria-label="Show reasoning & tools"
              >
                <MessageSquarePlus className="h-3.5 w-3.5" />
              </Button>
            </TooltipTrigger>
            <TooltipContent>Show reasoning & tools</TooltipContent>
          </Tooltip>
        </div>
        <div className="h-px flex-1 bg-border" />
      </div>

      {/* Response cards with optional scroll navigation */}
      <div className={useHorizontalLayout ? "relative" : undefined}>
        <div
          ref={useHorizontalLayout ? scrollContainerRef : undefined}
          className={layoutClasses}
          role={useHorizontalLayout ? "region" : undefined}
          aria-label={useHorizontalLayout ? "Model responses" : undefined}
          tabIndex={useHorizontalLayout ? 0 : undefined}
          onKeyDown={useHorizontalLayout ? handleScrollKeyDown : undefined}
        >
          {displayedResponses.map((response, index) => {
            const instanceId = getInstanceId(response);
            // Use groupId if provided, otherwise use timestamp as fallback
            const effectiveGroupId = groupId ?? timestamp.toISOString();
            return (
              <ModelResponseCard
                key={instanceId}
                response={response}
                model={response.model}
                groupId={effectiveGroupId}
                instanceId={instanceId}
                messageId={response.messageId}
                label={response.label}
                index={index}
                isExpanded={expandedModel === instanceId}
                onExpand={isMultiResponse ? handleExpand : undefined}
                onSelectBest={onSelectBest ? handleSelectBest : undefined}
                onRegenerate={onRegenerate ? handleRegenerate : undefined}
                onHide={onHide ? handleHide : undefined}
                onSaveEdit={onSaveEdit ? handleSaveEdit : undefined}
                isSelectedBest={selectedBest === instanceId}
                showSelectBest={showSelectBest}
                actionConfig={actionsConfig}
                useHorizontalLayout={useHorizontalLayout}
              />
            );
          })}
        </div>

        {/* Scroll navigation arrows and edge gradients */}
        {useHorizontalLayout && canScrollLeft && (
          <>
            <div className="pointer-events-none absolute left-0 top-0 bottom-0 w-4 bg-gradient-to-r from-background to-transparent z-10" />
            <button
              onClick={() => handleScrollBy("left")}
              className="absolute left-1 top-20 z-20 flex h-8 w-8 items-center justify-center rounded-full border bg-background shadow-md transition-opacity hover:bg-muted"
              aria-label="Scroll left"
            >
              <ChevronLeft className="h-4 w-4" />
            </button>
          </>
        )}
        {useHorizontalLayout && canScrollRight && (
          <>
            <div className="pointer-events-none absolute right-0 top-0 bottom-0 w-4 bg-gradient-to-l from-background to-transparent z-10" />
            <button
              onClick={() => handleScrollBy("right")}
              className="absolute right-1 top-20 z-20 flex h-8 w-8 items-center justify-center rounded-full border bg-background shadow-md transition-opacity hover:bg-muted"
              aria-label="Scroll right"
            >
              <ChevronRight className="h-4 w-4" />
            </button>
          </>
        )}
      </div>

      {/* Hidden responses indicator */}
      {hasHiddenResponses && (
        <div className="mt-3 flex items-center gap-2">
          <div className="h-px flex-1 bg-border/50" />
          <Dropdown>
            <DropdownTrigger asChild showChevron={false}>
              <button className="inline-flex items-center gap-1.5 rounded-full border border-dashed border-muted-foreground/30 bg-muted/30 px-3 py-1.5 text-xs text-muted-foreground transition-colors hover:border-muted-foreground/50 hover:bg-muted/50 hover:text-foreground">
                <EyeOff className="h-3 w-3" />
                <span>
                  {hiddenResponses.length} hidden{" "}
                  {hiddenResponses.length === 1 ? "response" : "responses"}
                </span>
              </button>
            </DropdownTrigger>
            <DropdownContent align="center" className="min-w-[200px]">
              {hiddenResponses.length > 1 && (
                <>
                  <DropdownItem onClick={handleShowAllHidden}>
                    <Eye className="h-4 w-4 mr-2" />
                    Show all ({hiddenResponses.length})
                  </DropdownItem>
                  <DropdownSeparator />
                </>
              )}
              {hiddenResponses.map((hidden) => {
                const displayName = hidden.label || getModelDisplayName(hidden.model);
                const style = getModelStyle(hidden.model);
                return (
                  <DropdownItem
                    key={hidden.instanceId}
                    onClick={() => handleShowHidden(hidden.instanceId)}
                  >
                    <div className="flex items-center gap-2">
                      <Eye className="h-4 w-4 shrink-0" />
                      <span
                        className={cn(
                          "inline-flex items-center rounded px-1.5 py-0.5 text-xs font-medium truncate",
                          style.bgColor,
                          style.color
                        )}
                      >
                        {displayName}
                      </span>
                    </div>
                  </DropdownItem>
                );
              })}
            </DropdownContent>
          </Dropdown>
          <div className="h-px flex-1 bg-border/50" />
        </div>
      )}
    </div>
  );
}

/**
 * Custom Memo Comparator for MultiModelResponse
 *
 * This comparator is critical for performance. It checks props in order of cost:
 * 1. Primitives (fast, O(1))
 * 2. Callback references (fast, O(1))
 * 3. ActionConfig object (shallow comparison)
 * 4. Responses array (most expensive, O(n))
 *
 * IMPORTANT: The parent (ChatMessageList) MUST provide stable callback references
 * via useCallback, otherwise this comparator will always return false and
 * memoization will be useless.
 */
function areMultiModelResponsePropsEqual(
  prev: MultiModelResponseProps,
  next: MultiModelResponseProps
): boolean {
  // Check primitive props first (cheapest)
  if (prev.groupId !== next.groupId) return false;
  if (prev.selectedBest !== next.selectedBest) return false;
  if (prev.timestamp.getTime() !== next.timestamp.getTime()) return false;
  if (prev.historyMode !== next.historyMode) return false;
  if (prev.forceStacked !== next.forceStacked) return false;

  // Check callback identity - parent MUST use useCallback for stable refs
  if (prev.onSelectBest !== next.onSelectBest) return false;
  if (prev.onRegenerate !== next.onRegenerate) return false;
  if (prev.onHide !== next.onHide) return false;
  if (prev.onSaveEdit !== next.onSaveEdit) return false;
  if (prev.onShowHidden !== next.onShowHidden) return false;

  // Check actionConfig (shallow comparison of object properties)
  if (prev.actionConfig !== next.actionConfig) {
    if (!prev.actionConfig || !next.actionConfig) return false;
    if (prev.actionConfig.showSelectBest !== next.actionConfig.showSelectBest) return false;
    if (prev.actionConfig.showRegenerate !== next.actionConfig.showRegenerate) return false;
    if (prev.actionConfig.showCopy !== next.actionConfig.showCopy) return false;
    if (prev.actionConfig.showExpand !== next.actionConfig.showExpand) return false;
    if (prev.actionConfig.showHide !== next.actionConfig.showHide) return false;
    if (prev.actionConfig.showSpeak !== next.actionConfig.showSpeak) return false;
  }

  // Check responses array last (most expensive - O(n) iteration)
  if (prev.responses.length !== next.responses.length) return false;
  for (let i = 0; i < prev.responses.length; i++) {
    const prevR = prev.responses[i];
    const nextR = next.responses[i];
    if (prevR.model !== nextR.model) return false;
    if (prevR.instanceId !== nextR.instanceId) return false;
    if (prevR.label !== nextR.label) return false;
    if (prevR.content !== nextR.content) return false;
    if (prevR.reasoningContent !== nextR.reasoningContent) return false;
    if ((prevR.completedRounds?.length ?? 0) !== (nextR.completedRounds?.length ?? 0)) return false;
    // Check if tool executions changed within completed rounds
    if (prevR.completedRounds && nextR.completedRounds) {
      for (let j = 0; j < prevR.completedRounds.length; j++) {
        if (!!prevR.completedRounds[j].toolExecution !== !!nextR.completedRounds[j].toolExecution)
          return false;
      }
    }
    if (prevR.isStreaming !== nextR.isStreaming) return false;
    if (prevR.error !== nextR.error) return false;
    if (prevR.usage?.totalTokens !== nextR.usage?.totalTokens) return false;
    if (prevR.usage?.reasoningTokens !== nextR.usage?.reasoningTokens) return false;
    if (prevR.usage?.cost !== nextR.usage?.cost) return false;
    // Feedback flips (rating, "select as best") — these change badges in the
    // header; without a check the user has to scroll/click to see the new
    // state.
    if (prevR.feedback?.rating !== nextR.feedback?.rating) return false;
    if (prevR.feedback?.selectedAsBest !== nextR.feedback?.selectedAsBest) return false;
    // Mode metadata (e.g., router model swap on regenerate) drives the
    // routing badge.
    if (prevR.modeMetadata !== nextR.modeMetadata) return false;
    // Check citations (compare length as a quick check)
    if ((prevR.citations?.length ?? 0) !== (nextR.citations?.length ?? 0)) return false;
    // Check artifacts (compare length as a quick check)
    if ((prevR.artifacts?.length ?? 0) !== (nextR.artifacts?.length ?? 0)) return false;
    // Check tool execution rounds (compare length and total executions as quick check)
    const prevRoundsLen = prevR.toolExecutionRounds?.length ?? 0;
    const nextRoundsLen = nextR.toolExecutionRounds?.length ?? 0;
    if (prevRoundsLen !== nextRoundsLen) return false;
    if (prevRoundsLen > 0) {
      // Also check total executions to detect updates within rounds
      const prevExecs = prevR.toolExecutionRounds!.reduce((sum, r) => sum + r.executions.length, 0);
      const nextExecs = nextR.toolExecutionRounds!.reduce((sum, r) => sum + r.executions.length, 0);
      if (prevExecs !== nextExecs) return false;
    }
  }

  // Check hidden responses array
  const prevHidden = prev.hiddenResponses ?? [];
  const nextHidden = next.hiddenResponses ?? [];
  if (prevHidden.length !== nextHidden.length) return false;
  for (let i = 0; i < prevHidden.length; i++) {
    if (prevHidden[i].instanceId !== nextHidden[i].instanceId) return false;
    if (prevHidden[i].model !== nextHidden[i].model) return false;
    if (prevHidden[i].label !== nextHidden[i].label) return false;
  }

  return true;
}

export const MultiModelResponse = memo(
  MultiModelResponseComponent,
  areMultiModelResponsePropsEqual
);
