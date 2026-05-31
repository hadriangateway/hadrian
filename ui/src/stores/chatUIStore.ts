import { create } from "zustand";

import type {
  ModelParameters,
  ResponseActionConfig,
  HistoryMode,
  ConversationMode,
  ModeConfig,
} from "@/components/chat-types";
import type { FileType } from "@/services/duckdb";
import { DEFAULT_MODE_CONFIG } from "@/components/chat-types";
import type { PlaybackState } from "@/hooks/useAudioPlayback";
import { DEFAULT_TTS_VOICE, DEFAULT_TTS_SPEED, TTS_VOICES } from "@/hooks/useAudioPlayback";
import type { Voice } from "@/api/generated/types.gen";

/**
 * Chat UI Store - Ephemeral UI State
 *
 * ## Architecture Overview
 *
 * This store manages **ephemeral UI state** that is separate from:
 * - `conversationStore` (persistent messages)
 * - `streamingStore` (in-flight streaming data)
 *
 * ## What Belongs Here
 *
 * - View preferences (grid vs stacked layout)
 * - Expanded/collapsed state
 * - Scroll position tracking
 * - Per-session settings that don't need persistence
 * - UI-only state like modal open/closed
 *
 * ## Performance Characteristics
 *
 * - **Low-frequency updates**: UI interactions are human-speed, not token-speed
 * - **No persistence overhead**: State resets on page refresh (intentional)
 * - **Independent re-renders**: Components subscribed here don't re-render on message changes
 *
 * ## Re-render Isolation
 *
 * Example: User toggles view mode from "grid" to "stacked":
 * ```
 * setViewMode("stacked")
 *     │
 *     ▼
 * Only components using useViewMode() re-render
 *     ├── MultiModelResponse  ✅ RE-RENDERS (changes layout)
 *     ├── ChatMessageList     ❌ NO RE-RENDER (doesn't use viewMode)
 *     └── ChatMessage         ❌ NO RE-RENDER (doesn't use viewMode)
 * ```
 */

/** View mode for multi-model responses */
export type ViewMode = "grid" | "stacked";

/** Column schema information for a data file */
export interface DataFileColumn {
  /** Column name */
  name: string;
  /** Column data type (e.g., VARCHAR, INTEGER, DOUBLE) */
  type: string;
}

/** Table schema for database files */
export interface DataFileTable {
  /** Table name */
  tableName: string;
  /** Schema name (e.g. "main") */
  schemaName: string;
  /** Columns in the table */
  columns: DataFileColumn[];
}

/** Metadata for a data file registered with DuckDB */
export interface DataFile {
  /** Unique identifier */
  id: string;
  /** Original filename */
  name: string;
  /** File type */
  type: FileType;
  /** File size in bytes */
  size: number;
  /** Upload timestamp */
  uploadedAt: number;
  /** Whether the file is registered with DuckDB */
  registered: boolean;
  /** Error message if registration failed */
  error?: string;
  /** Column schema for flat files (CSV, Parquet, JSON) */
  columns?: DataFileColumn[];
  /** Table schemas for database files (DuckDB) */
  tables?: DataFileTable[];
  /** Database alias for attached databases */
  dbName?: string;
}

// Re-export types for convenience
export type { HistoryMode, ConversationMode, ModeConfig } from "@/components/chat-types";
export { CONVERSATION_MODES, getModeMetadata, DEFAULT_MODE_CONFIG } from "@/components/chat-types";

interface ChatUIState {
  /** Current view mode for multi-model responses */
  viewMode: ViewMode;
  /** Currently expanded model (for stacked view or focus) */
  expandedModel: string | null;
  /** Whether user has scrolled up (disables auto-scroll) */
  userHasScrolledUp: boolean;
  /** System prompt for the conversation */
  systemPrompt: string;
  /** History mode: "all" sends all model responses, "same-model" sends only same-model history */
  historyMode: HistoryMode;
  /** Per-model parameter overrides */
  perModelSettings: Record<string, ModelParameters>;
  /** Models that are temporarily disabled (not queried) */
  disabledModels: string[];
  /** Map of message group ID to selected best model */
  selectedBestResponses: Record<string, string>;
  /** Configuration for which action buttons to show */
  actionConfig: ResponseActionConfig;
  /** Whether settings modal is open */
  settingsModalOpen: boolean;
  /** Whether MCP config modal is open */
  mcpConfigModalOpen: boolean;
  /** Current conversation mode for multi-model interactions */
  conversationMode: ConversationMode;
  /** Mode-specific configuration parameters */
  modeConfig: ModeConfig;
  /** Attached vector store IDs for file_search tool (RAG) */
  vectorStoreIds: string[];
  /**
   * Enable client-side tool execution for file_search.
   * When true, the frontend detects tool calls in the SSE stream,
   * executes the search API directly, and sends results back to continue.
   * When false (default), the backend middleware handles tool execution.
   */
  clientSideRAG: boolean;
  /**
   * Enabled tool IDs for this conversation.
   * Tools must be enabled here AND have their requirements met (e.g., vector stores)
   * to be available. Empty array means no tools are enabled.
   */
  enabledTools: string[];
  /**
   * IDs of skills the user has enabled for this session. Enabled skills are
   * listed in the `Skill` tool's description so the model can auto-invoke
   * them (agentskills.io). Skills with `disable_model_invocation: true` or
   * `user_invocable: false` are further filtered in useChat.
   */
  enabledSkillIds: string[];
  /**
   * Id of a skill the user just invoked via slash command whose `SKILL.md`
   * should be seeded directly into the next outgoing request, so the skill
   * loads deterministically instead of relying on the model to call the tool.
   * Consumed and cleared by `sendMessage`.
   */
  pendingSkillId: string | null;
  /**
   * Data files registered with DuckDB for SQL queries.
   * Files are registered in-memory and reset on page reload.
   */
  dataFiles: DataFile[];
  /**
   * Maximum number of tool execution iterations before stopping.
   * Prevents infinite loops when client-side tool execution is enabled.
   */
  maxToolIterations: number;
  /**
   * Whether to capture raw SSE events during streaming for debugging.
   * Disabled by default as it can generate significant data.
   */
  captureRawSSEEvents: boolean;
  /**
   * Set of hidden response IDs in format "groupId:instanceId".
   * Used to hide individual responses without affecting future queries.
   */
  hiddenResponseIds: Set<string>;
  /**
   * ID of the response currently playing TTS audio, in format "groupId:instanceId".
   * Only one response can be playing at a time.
   */
  ttsActiveResponseId: string | null;
  /**
   * Current TTS playback state for the active response.
   */
  ttsPlaybackState: PlaybackState;
  /**
   * Preferred TTS voice for speech generation.
   */
  ttsVoice: Voice;
  /**
   * Preferred TTS playback speed (0.25 to 4.0).
   */
  ttsSpeed: number;
  /**
   * Text selected for quoting in chat input.
   * Contains the message context and selected text.
   */
  quotedText: {
    /** ID of the message containing the quoted text */
    messageId: string;
    /** Instance ID for multi-model responses (optional for user messages) */
    instanceId?: string;
    /** The selected text to quote */
    text: string;
  } | null;
  /**
   * Whether widescreen mode is enabled.
   * When true, removes max-width constraints from the chat UI.
   */
  widescreenMode: boolean;
  /**
   * ID of the message currently being edited inline.
   * Only one message can be edited at a time.
   */
  editingMessageId: string | null;
  /**
   * Pending prompt to insert into chat input.
   * Used by example prompts feature - replaces current input content.
   */
  pendingPrompt: string | null;
  /**
   * Default model for sub-agent tool.
   * When null, uses the current streaming model as fallback.
   */
  subAgentModel: string | null;
  /**
   * Whether compact mode is enabled for model responses.
   * Hides reasoning sections, tool execution details, and collapses
   * rounds without content to minimal "Thinking" / "Processing" indicators.
   */
  compactMode: boolean;

  // --- Agent mode (shell tool + container) ---
  // The shell tool is enabled via the `agent` entry in `enabledTools`
  // (ToolsBar); these fields configure the container a new conversation
  // provisions. The conversation then reuses that container until it expires
  // (handled in useChat), so there's no manual container picker.
  /** Memory ceiling (OpenAI string, e.g. "512m"/"1g"). Empty = operator default. */
  agentMemoryLimit: string;
  /** Idle TTL in minutes for a new container. Null = operator default. */
  agentExpiresAfterMinutes: number | null;
  /**
   * Egress allowlist (comma/newline separated). Defaults to `*` (any host,
   * subject to the operator's `allowed_egress_hosts` ceiling). Empty = deny-all.
   */
  agentAllowedDomains: string;
  /**
   * Let the model search for tools / defer-load them (`tool_search`).
   * Keeps context small when many MCP tools are attached.
   */
  toolSearchEnabled: boolean;
  /**
   * Ranking strategy override for tool search. `"default"` omits the field
   * (deployment default applies). `"semantic"`/`"hybrid"` need an embedding
   * provider — the gateway rejects them with 400 otherwise.
   */
  toolSearchRanker: "default" | "hybrid" | "semantic" | "lexical";
}

interface ChatUIActions {
  /** Set view mode */
  setViewMode: (mode: ViewMode) => void;
  /** Set expanded model */
  setExpandedModel: (model: string | null) => void;
  /** Set user scroll state */
  setUserHasScrolledUp: (scrolledUp: boolean) => void;
  /** Set system prompt */
  setSystemPrompt: (prompt: string) => void;
  /** Set history mode */
  setHistoryMode: (mode: HistoryMode) => void;
  /** Update settings for a specific model */
  setModelSettings: (modelId: string, params: ModelParameters) => void;
  /** Set all per-model settings */
  setAllModelSettings: (settings: Record<string, ModelParameters>) => void;
  /** Toggle a model's disabled state */
  toggleModelDisabled: (modelId: string) => void;
  /** Set disabled models list */
  setDisabledModels: (models: string[]) => void;
  /** Set selected best response for a message group */
  setSelectedBest: (messageGroupId: string, model: string | null) => void;
  /** Clear selected best responses */
  clearSelectedBestResponses: () => void;
  /** Set action config */
  setActionConfig: (config: ResponseActionConfig) => void;
  /** Set settings modal open state */
  setSettingsModalOpen: (open: boolean) => void;
  /** Set MCP config modal open state */
  setMCPConfigModalOpen: (open: boolean) => void;
  /** Set conversation mode */
  setConversationMode: (mode: ConversationMode) => void;
  /** Update mode-specific configuration */
  setModeConfig: (config: Partial<ModeConfig>) => void;
  /** Reset mode config to defaults */
  resetModeConfig: () => void;
  /** Reset UI state for new conversation */
  resetUIState: () => void;
  /** Set all attached vector store IDs */
  setVectorStoreIds: (ids: string[]) => void;
  /** Add a vector store to the attached list */
  addVectorStoreId: (id: string) => void;
  /** Remove a vector store from the attached list */
  removeVectorStoreId: (id: string) => void;
  /** Set client-side RAG execution mode */
  setClientSideRAG: (enabled: boolean) => void;
  /** Set all enabled tool IDs */
  setEnabledTools: (tools: string[]) => void;
  /** Toggle a tool's enabled state */
  toggleTool: (toolId: string) => void;
  /** Enable a specific tool */
  enableTool: (toolId: string) => void;
  /** Toggle a skill's enabled state for this session. */
  toggleSkill: (skillId: string) => void;
  /**
   * Mark a skill as explicitly user-invoked (via slash command): enables it and
   * queues its `SKILL.md` to be seeded directly into the next request.
   */
  markSkillUserInvoked: (skillId: string) => void;
  /** Clear the queued pending-skill seed (after it's consumed or dismissed). */
  clearPendingSkill: () => void;
  /** Replace the full set of enabled skills. */
  setEnabledSkillIds: (ids: string[]) => void;
  /** Disable a specific tool */
  disableTool: (toolId: string) => void;
  /** Add a data file */
  addDataFile: (file: DataFile) => void;
  /** Remove a data file by ID */
  removeDataFile: (fileId: string) => void;
  /** Update a data file's registration status and optionally its schema */
  updateDataFileStatus: (
    fileId: string,
    registered: boolean,
    error?: string,
    schema?: {
      columns?: DataFileColumn[];
      tables?: DataFileTable[];
      dbName?: string;
    }
  ) => void;
  /** Clear all data files */
  clearDataFiles: () => void;
  /** Set maximum tool execution iterations */
  setMaxToolIterations: (iterations: number) => void;
  /** Set whether to capture raw SSE events */
  setCaptureRawSSEEvents: (capture: boolean) => void;
  /** Hide a specific response by groupId and instanceId */
  hideResponse: (groupId: string, instanceId: string) => void;
  /** Show a previously hidden response */
  showResponse: (groupId: string, instanceId: string) => void;
  /** Toggle visibility of a response */
  toggleResponseVisibility: (groupId: string, instanceId: string) => void;
  /** Check if a response is hidden */
  isResponseHidden: (groupId: string, instanceId: string) => boolean;
  /** Clear all hidden responses */
  clearHiddenResponses: () => void;
  /** Set the active TTS response and playback state */
  setTTSActive: (groupId: string, instanceId: string, state: PlaybackState) => void;
  /** Update only the TTS playback state (for the currently active response) */
  setTTSPlaybackState: (state: PlaybackState) => void;
  /** Stop TTS playback and clear the active response */
  stopTTS: () => void;
  /** Set the preferred TTS voice */
  setTTSVoice: (voice: Voice) => void;
  /** Set the preferred TTS playback speed */
  setTTSSpeed: (speed: number) => void;
  /** Set the quoted text for chat input */
  setQuotedText: (quote: { messageId: string; instanceId?: string; text: string }) => void;
  /** Clear the quoted text */
  clearQuotedText: () => void;
  /** Set widescreen mode */
  setWidescreenMode: (enabled: boolean) => void;
  /** Toggle widescreen mode */
  toggleWidescreenMode: () => void;
  /** Start editing a message */
  startEditing: (messageId: string) => void;
  /** Stop editing (cancel or after save) */
  stopEditing: () => void;
  /** Set pending prompt to insert into chat input */
  setPendingPrompt: (prompt: string) => void;
  /** Clear the pending prompt */
  clearPendingPrompt: () => void;
  /** Set the default model for sub-agent tool */
  setSubAgentModel: (model: string | null) => void;
  /** Set compact mode */
  setCompactMode: (enabled: boolean) => void;
  /** Toggle compact mode */
  toggleCompactMode: () => void;

  // --- Agent mode setters ---
  setAgentMemoryLimit: (value: string) => void;
  setAgentExpiresAfterMinutes: (minutes: number | null) => void;
  setAgentAllowedDomains: (value: string) => void;
  setToolSearchEnabled: (enabled: boolean) => void;
  setToolSearchRanker: (ranker: "default" | "hybrid" | "semantic" | "lexical") => void;
}

export type ChatUIStore = ChatUIState & ChatUIActions;

const defaultActionConfig: ResponseActionConfig = {
  showSelectBest: true,
  showRegenerate: true,
  showCopy: true,
  showExpand: true,
  showHide: true,
  showSpeak: true,
};

function loadViewMode(): ViewMode {
  try {
    const stored = localStorage.getItem("hadrian:viewMode");
    if (stored === "grid" || stored === "stacked") return stored;
  } catch {
    // localStorage unavailable (SSR, privacy mode, etc.)
  }
  return "grid";
}

function loadCompactMode(): boolean {
  try {
    return localStorage.getItem("hadrian:compactMode") !== "false";
  } catch {
    return true;
  }
}

const initialState: ChatUIState = {
  viewMode: loadViewMode(),
  expandedModel: null,
  userHasScrolledUp: false,
  systemPrompt: "",
  historyMode: "all",
  perModelSettings: {},
  disabledModels: [],
  selectedBestResponses: {},
  actionConfig: defaultActionConfig,
  settingsModalOpen: false,
  mcpConfigModalOpen: false,
  conversationMode: "multiple",
  modeConfig: { ...DEFAULT_MODE_CONFIG },
  vectorStoreIds: [],
  clientSideRAG: false,
  enabledTools: [],
  enabledSkillIds: [],
  pendingSkillId: null,
  dataFiles: [],
  maxToolIterations: 25,
  captureRawSSEEvents: false,
  hiddenResponseIds: new Set<string>(),
  ttsActiveResponseId: null,
  ttsPlaybackState: "idle",
  ttsVoice: DEFAULT_TTS_VOICE,
  ttsSpeed: DEFAULT_TTS_SPEED,
  quotedText: null,
  widescreenMode: false,
  editingMessageId: null,
  pendingPrompt: null,
  subAgentModel: null,
  compactMode: loadCompactMode(),
  agentMemoryLimit: "",
  agentExpiresAfterMinutes: null,
  agentAllowedDomains: "*",
  toolSearchEnabled: false,
  toolSearchRanker: "default",
};

export const useChatUIStore = create<ChatUIStore>((set) => ({
  ...initialState,

  setViewMode: (mode) => {
    try {
      localStorage.setItem("hadrian:viewMode", mode);
    } catch {
      // localStorage unavailable
    }
    set({ viewMode: mode });
  },

  setExpandedModel: (model) => set({ expandedModel: model }),

  setUserHasScrolledUp: (scrolledUp) => set({ userHasScrolledUp: scrolledUp }),

  setSystemPrompt: (prompt) => set({ systemPrompt: prompt }),

  setHistoryMode: (mode) => set({ historyMode: mode }),

  setModelSettings: (modelId, params) =>
    set((state) => ({
      perModelSettings: {
        ...state.perModelSettings,
        [modelId]: params,
      },
    })),

  setAllModelSettings: (settings) => set({ perModelSettings: settings }),

  toggleModelDisabled: (modelId) =>
    set((state) => {
      const isDisabled = state.disabledModels.includes(modelId);
      return {
        disabledModels: isDisabled
          ? state.disabledModels.filter((m) => m !== modelId)
          : [...state.disabledModels, modelId],
      };
    }),

  setDisabledModels: (models) => set({ disabledModels: models }),

  setSelectedBest: (messageGroupId, model) =>
    set((state) => {
      if (model === null) {
        const { [messageGroupId]: _, ...rest } = state.selectedBestResponses;
        return { selectedBestResponses: rest };
      }
      return {
        selectedBestResponses: {
          ...state.selectedBestResponses,
          [messageGroupId]: model,
        },
      };
    }),

  clearSelectedBestResponses: () => set({ selectedBestResponses: {} }),

  setActionConfig: (config) => set({ actionConfig: config }),

  setSettingsModalOpen: (open) => set({ settingsModalOpen: open }),

  setMCPConfigModalOpen: (open) => set({ mcpConfigModalOpen: open }),

  setConversationMode: (mode) => set({ conversationMode: mode }),

  setModeConfig: (config) =>
    set((state) => ({
      modeConfig: {
        ...state.modeConfig,
        ...config,
      },
    })),

  resetModeConfig: () => set({ modeConfig: { ...DEFAULT_MODE_CONFIG } }),

  resetUIState: () =>
    set({
      ...initialState,
      // Preserve some settings across conversations
      actionConfig: initialState.actionConfig,
      // Preserve conversation mode preference
      conversationMode: initialState.conversationMode,
      // Ensure fresh Set instance for hidden responses
      hiddenResponseIds: new Set<string>(),
    }),

  setVectorStoreIds: (ids) =>
    set((state) => {
      // Auto-enable file_search when adding vector stores for the first time
      const hadNoStores = state.vectorStoreIds.length === 0;
      const hasNewStores = ids.length > 0;
      const shouldEnableFileSearch =
        hadNoStores && hasNewStores && !state.enabledTools.includes("file_search");

      return {
        vectorStoreIds: ids,
        enabledTools: shouldEnableFileSearch
          ? [...state.enabledTools, "file_search"]
          : state.enabledTools,
      };
    }),

  addVectorStoreId: (id) =>
    set((state) => {
      if (state.vectorStoreIds.includes(id)) {
        return state;
      }
      // Auto-enable file_search when adding first vector store
      const isFirstStore = state.vectorStoreIds.length === 0;
      const shouldEnableFileSearch = isFirstStore && !state.enabledTools.includes("file_search");

      return {
        vectorStoreIds: [...state.vectorStoreIds, id],
        enabledTools: shouldEnableFileSearch
          ? [...state.enabledTools, "file_search"]
          : state.enabledTools,
      };
    }),

  removeVectorStoreId: (id) =>
    set((state) => ({
      vectorStoreIds: state.vectorStoreIds.filter((vsId) => vsId !== id),
    })),

  setClientSideRAG: (enabled) => set({ clientSideRAG: enabled }),

  setEnabledTools: (tools) => set({ enabledTools: tools }),

  toggleTool: (toolId) =>
    set((state) => ({
      enabledTools: state.enabledTools.includes(toolId)
        ? state.enabledTools.filter((t) => t !== toolId)
        : [...state.enabledTools, toolId],
    })),

  enableTool: (toolId) =>
    set((state) => ({
      enabledTools: state.enabledTools.includes(toolId)
        ? state.enabledTools
        : [...state.enabledTools, toolId],
    })),

  disableTool: (toolId) =>
    set((state) => ({
      enabledTools: state.enabledTools.filter((t) => t !== toolId),
    })),

  toggleSkill: (skillId) =>
    set((state) => {
      const willDisable = state.enabledSkillIds.includes(skillId);
      return {
        enabledSkillIds: willDisable
          ? state.enabledSkillIds.filter((id) => id !== skillId)
          : [...state.enabledSkillIds, skillId],
        // Disabling a skill also drops any queued seed for it.
        pendingSkillId:
          willDisable && state.pendingSkillId === skillId ? null : state.pendingSkillId,
      };
    }),

  markSkillUserInvoked: (skillId) =>
    set((state) => ({
      enabledSkillIds: state.enabledSkillIds.includes(skillId)
        ? state.enabledSkillIds
        : [...state.enabledSkillIds, skillId],
      pendingSkillId: skillId,
    })),

  clearPendingSkill: () => set({ pendingSkillId: null }),

  setEnabledSkillIds: (ids) => set({ enabledSkillIds: ids }),

  addDataFile: (file) =>
    set((state) => ({
      dataFiles: [...state.dataFiles, file],
    })),

  removeDataFile: (fileId) =>
    set((state) => ({
      dataFiles: state.dataFiles.filter((f) => f.id !== fileId),
    })),

  updateDataFileStatus: (fileId, registered, error, schema) =>
    set((state) => ({
      dataFiles: state.dataFiles.map((f) =>
        f.id === fileId
          ? {
              ...f,
              registered,
              error,
              columns: schema?.columns,
              tables: schema?.tables,
              dbName: schema?.dbName,
            }
          : f
      ),
    })),

  clearDataFiles: () => set({ dataFiles: [] }),

  setMaxToolIterations: (iterations) => set({ maxToolIterations: iterations }),

  setCaptureRawSSEEvents: (capture) => set({ captureRawSSEEvents: capture }),

  hideResponse: (groupId, instanceId) =>
    set((state) => {
      const key = `${groupId}:${instanceId}`;
      if (state.hiddenResponseIds.has(key)) return state;
      const newSet = new Set(state.hiddenResponseIds);
      newSet.add(key);
      return { hiddenResponseIds: newSet };
    }),

  showResponse: (groupId, instanceId) =>
    set((state) => {
      const key = `${groupId}:${instanceId}`;
      if (!state.hiddenResponseIds.has(key)) return state;
      const newSet = new Set(state.hiddenResponseIds);
      newSet.delete(key);
      return { hiddenResponseIds: newSet };
    }),

  toggleResponseVisibility: (groupId, instanceId) =>
    set((state) => {
      const key = `${groupId}:${instanceId}`;
      const newSet = new Set(state.hiddenResponseIds);
      if (newSet.has(key)) {
        newSet.delete(key);
      } else {
        newSet.add(key);
      }
      return { hiddenResponseIds: newSet };
    }),

  isResponseHidden: (groupId, instanceId): boolean => {
    const key = `${groupId}:${instanceId}`;
    return useChatUIStore.getState().hiddenResponseIds.has(key);
  },

  clearHiddenResponses: () => set({ hiddenResponseIds: new Set<string>() }),

  setTTSActive: (groupId, instanceId, state) =>
    set({ ttsActiveResponseId: `${groupId}:${instanceId}`, ttsPlaybackState: state }),

  setTTSPlaybackState: (state) => set({ ttsPlaybackState: state }),

  stopTTS: () => set({ ttsActiveResponseId: null, ttsPlaybackState: "idle" }),

  setTTSVoice: (voice) => {
    // Validate voice is in the allowed list
    if (TTS_VOICES.includes(voice)) {
      set({ ttsVoice: voice });
    }
  },

  setTTSSpeed: (speed) => {
    // Clamp speed to valid range (0.25 to 4.0)
    const clampedSpeed = Math.max(0.25, Math.min(4.0, speed));
    set({ ttsSpeed: clampedSpeed });
  },

  setQuotedText: (quote) => set({ quotedText: quote }),

  clearQuotedText: () => set({ quotedText: null }),

  setWidescreenMode: (enabled) => set({ widescreenMode: enabled }),

  toggleWidescreenMode: () => set((state) => ({ widescreenMode: !state.widescreenMode })),

  startEditing: (messageId) => set({ editingMessageId: messageId }),

  stopEditing: () => set({ editingMessageId: null }),

  setPendingPrompt: (prompt) => set({ pendingPrompt: prompt }),

  clearPendingPrompt: () => set({ pendingPrompt: null }),

  setSubAgentModel: (model) => set({ subAgentModel: model }),

  setCompactMode: (enabled) => {
    try {
      localStorage.setItem("hadrian:compactMode", String(enabled));
    } catch {
      // localStorage unavailable
    }
    set({ compactMode: enabled });
  },

  toggleCompactMode: () =>
    set((state) => {
      const next = !state.compactMode;
      try {
        localStorage.setItem("hadrian:compactMode", String(next));
      } catch {
        // localStorage unavailable
      }
      return { compactMode: next };
    }),

  setAgentMemoryLimit: (value) => set({ agentMemoryLimit: value }),
  setAgentExpiresAfterMinutes: (minutes) => set({ agentExpiresAfterMinutes: minutes }),
  setAgentAllowedDomains: (value) => set({ agentAllowedDomains: value }),
  setToolSearchEnabled: (enabled) => set({ toolSearchEnabled: enabled }),
  setToolSearchRanker: (ranker) => set({ toolSearchRanker: ranker }),
}));

/**
 * Surgical Selectors - Subscribe Only to What You Need
 *
 * Each selector returns a specific slice of UI state. Components using these
 * selectors only re-render when their specific slice changes.
 */

/** Get view mode only - used by MultiModelResponse for layout decisions */
export const useViewMode = () => useChatUIStore((state: ChatUIState) => state.viewMode);

/** Get expanded model only - used by MultiModelResponse to show/hide cards */
export const useExpandedModel = () => useChatUIStore((state: ChatUIState) => state.expandedModel);

/** Get scroll state only - used by auto-scroll logic */
export const useUserHasScrolledUp = () =>
  useChatUIStore((state: ChatUIState) => state.userHasScrolledUp);

/** Get system prompt - used by useChat when building API request */
export const useSystemPrompt = () => useChatUIStore((state: ChatUIState) => state.systemPrompt);

/** Get history mode - controls whether models see each other's responses */
export const useHistoryMode = () => useChatUIStore((state: ChatUIState) => state.historyMode);

/** Get disabled models - filters which models are queried/displayed */
export const useDisabledModels = () => useChatUIStore((state: ChatUIState) => state.disabledModels);

/** Get selected best responses map - tracks user's "best" selection per message group */
export const useSelectedBestResponses = () =>
  useChatUIStore((state: ChatUIState) => state.selectedBestResponses);

/** Get settings for a specific model - per-model temperature, max tokens, etc. */
export const useModelSettings = (modelId: string) =>
  useChatUIStore((state: ChatUIState) => state.perModelSettings[modelId]);

/** Get all per-model settings */
export const usePerModelSettings = () =>
  useChatUIStore((state: ChatUIState) => state.perModelSettings);

/** Get action config - controls which action buttons are visible */
export const useActionConfig = () => useChatUIStore((state: ChatUIState) => state.actionConfig);

/** Get conversation mode - controls how multiple models interact */
export const useConversationMode = () =>
  useChatUIStore((state: ChatUIState) => state.conversationMode);

/** Get mode config - mode-specific parameters */
export const useModeConfig = () => useChatUIStore((state: ChatUIState) => state.modeConfig);

/** Get a specific mode config value */
export const useModeConfigValue = <K extends keyof ModeConfig>(key: K) =>
  useChatUIStore((state: ChatUIState) => state.modeConfig[key]);

/** Get attached vector store IDs - for file_search tool (RAG) */
export const useVectorStoreIds = () => useChatUIStore((state: ChatUIState) => state.vectorStoreIds);

/** Get client-side RAG execution mode */
export const useClientSideRAG = () => useChatUIStore((state: ChatUIState) => state.clientSideRAG);

/** Get enabled tools list */
export const useEnabledTools = () => useChatUIStore((state: ChatUIState) => state.enabledTools);

/** Check if a specific tool is enabled */
export const useIsToolEnabled = (toolId: string) =>
  useChatUIStore((state: ChatUIState) => state.enabledTools.includes(toolId));

/** Get the list of skills enabled for this session. */
export const useEnabledSkillIds = () =>
  useChatUIStore((state: ChatUIState) => state.enabledSkillIds);

/** Get data files registered with DuckDB */
export const useDataFiles = () => useChatUIStore((state: ChatUIState) => state.dataFiles);

/** Get maximum tool execution iterations */
export const useMaxToolIterations = () =>
  useChatUIStore((state: ChatUIState) => state.maxToolIterations);

/** Get whether to capture raw SSE events */
export const useCaptureRawSSEEvents = () =>
  useChatUIStore((state: ChatUIState) => state.captureRawSSEEvents);

/** Get the set of hidden response IDs */
export const useHiddenResponseIds = () =>
  useChatUIStore((state: ChatUIState) => state.hiddenResponseIds);

/** Check if a specific response is hidden */
export const useIsResponseHidden = (groupId: string, instanceId: string) =>
  useChatUIStore((state: ChatUIState) => state.hiddenResponseIds.has(`${groupId}:${instanceId}`));

/** Get count of hidden responses */
export const useHiddenResponseCount = () =>
  useChatUIStore((state: ChatUIState) => state.hiddenResponseIds.size);

/** Get the active TTS response ID */
export const useTTSActiveResponseId = () =>
  useChatUIStore((state: ChatUIState) => state.ttsActiveResponseId);

/** Get the TTS playback state */
export const useTTSPlaybackState = () =>
  useChatUIStore((state: ChatUIState) => state.ttsPlaybackState);

/** Check if a specific response is the active TTS response */
export const useIsTTSActive = (groupId: string, instanceId: string) =>
  useChatUIStore((state: ChatUIState) => state.ttsActiveResponseId === `${groupId}:${instanceId}`);

/** Get TTS playback state for a specific response (idle if not active) */
export const useTTSStateForResponse = (groupId: string, instanceId: string) =>
  useChatUIStore((state: ChatUIState) =>
    state.ttsActiveResponseId === `${groupId}:${instanceId}` ? state.ttsPlaybackState : "idle"
  );

/** Get the preferred TTS voice */
export const useTTSVoice = () => useChatUIStore((state: ChatUIState) => state.ttsVoice);

/** Get the preferred TTS playback speed */
export const useTTSSpeed = () => useChatUIStore((state: ChatUIState) => state.ttsSpeed);

/** Get the quoted text for chat input */
export const useQuotedText = () => useChatUIStore((state: ChatUIState) => state.quotedText);

/** Get widescreen mode state */
export const useWidescreenMode = () => useChatUIStore((state: ChatUIState) => state.widescreenMode);

/** Get the ID of the message currently being edited */
export const useEditingMessageId = () =>
  useChatUIStore((state: ChatUIState) => state.editingMessageId);

/** Check if a specific message is being edited */
export const useIsEditing = (messageId: string) =>
  useChatUIStore((state: ChatUIState) => state.editingMessageId === messageId);

/** Get the pending prompt for chat input */
export const usePendingPrompt = () => useChatUIStore((state: ChatUIState) => state.pendingPrompt);

/** Get the default model for sub-agent tool */
export const useSubAgentModel = () => useChatUIStore((state: ChatUIState) => state.subAgentModel);

/** Get compact mode state - hides reasoning/tools in model responses */
export const useCompactMode = () => useChatUIStore((state: ChatUIState) => state.compactMode);

/** Get MCP config modal open state */
export const useMCPConfigModalOpen = () =>
  useChatUIStore((state: ChatUIState) => state.mcpConfigModalOpen);

// --- Agent mode selectors ---
export const useAgentMemoryLimit = () =>
  useChatUIStore((state: ChatUIState) => state.agentMemoryLimit);
export const useAgentExpiresAfterMinutes = () =>
  useChatUIStore((state: ChatUIState) => state.agentExpiresAfterMinutes);
export const useAgentAllowedDomains = () =>
  useChatUIStore((state: ChatUIState) => state.agentAllowedDomains);
export const useToolSearchEnabled = () =>
  useChatUIStore((state: ChatUIState) => state.toolSearchEnabled);
export const useToolSearchRanker = () =>
  useChatUIStore((state: ChatUIState) => state.toolSearchRanker);
