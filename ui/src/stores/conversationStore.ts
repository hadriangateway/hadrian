import { useMemo } from "react";
import { create } from "zustand";

import type {
  Artifact,
  ChatMessage,
  Citation,
  CompletedRound,
  Conversation,
  HistoryMode,
  MessageModeMetadata,
  MessageUsage,
  ModelInstance,
  ResponseFeedback,
  ResponseFeedbackData,
  ToolExecutionRound,
} from "@/components/chat-types";
import { createDefaultInstance } from "@/components/chat-types";

/**
 * Conversation Store - Persistent Message State
 *
 * ## Architecture Overview
 *
 * This store manages **committed** conversation state - messages that have completed
 * streaming and should be persisted. It is separate from `streamingStore` which
 * handles ephemeral in-flight token data.
 *
 * ## Performance Characteristics
 *
 * - **Low-frequency updates**: Only updates when streaming completes or user interacts
 * - **Array-based messages**: Using an array (not Map) is fine for typical conversation
 *   sizes. The `findIndex` operations are O(n) but n is small (< 100 typically).
 *
 * ## Re-render Behavior
 *
 * When `addAssistantMessages` is called after streaming completes:
 * ```
 * addAssistantMessages([{ model, content, usage }])
 *     │
 *     ▼
 * messages array changes → components subscribed via useMessages() re-render
 *     ├── ChatMessageList  ✅ RE-RENDERS (maps over messages)
 *     │   └── Existing MemoizedMessage components  ❌ NO RE-RENDER (memo passes)
 *     │   └── New message component  ✅ MOUNTS
 *     └── Components using useIsStreaming()  ❌ NO RE-RENDER (different store)
 * ```
 *
 * ## Message Grouping
 *
 * Messages are stored as a flat array. The grouping (user message + assistant responses)
 * is computed at render time in `ChatMessageList` via `useMemo`. This keeps the store
 * simple while allowing flexible rendering.
 *
 * ## Key Files
 * - `useChat.ts` - Commits streaming content via `addAssistantMessages`
 * - `useConversationSync.ts` - Persists to localStorage/API
 * - `ChatMessageList.tsx` - Consumes messages via `useMessages()`
 */

interface ConversationState {
  /** All conversations (for sidebar list) */
  conversations: Conversation[];
  /** Currently active conversation */
  currentConversation: Conversation | null;
  /** Messages in the current conversation */
  messages: ChatMessage[];
  /**
   * Selected models for the current conversation.
   * @deprecated Use selectedInstances instead. Kept for backwards compatibility.
   */
  selectedModels: string[];
  /**
   * Selected model instances for the current conversation.
   * Each instance can have its own label and parameters.
   */
  selectedInstances: ModelInstance[];
  /**
   * Session-only accumulator for usage of assistant responses that were removed
   * from the conversation — discarded by edit-and-rerun (`deleteMessagesAfter`)
   * or overwritten by regeneration (`replaceAssistantMessage`). These tokens were
   * really spent but are no longer present in `messages`, so `useTotalUsage` would
   * otherwise undercount them. Combines per-response usage and mode overhead.
   *
   * Reset whenever the conversation changes (load/switch/clear); not persisted.
   */
  discardedUsage: MessageUsage;
  /** Number of discarded/regenerated assistant responses counted into `discardedUsage`. */
  discardedResponseCount: number;
}

interface ConversationActions {
  /** Set the list of conversations */
  setConversations: (conversations: Conversation[]) => void;
  /** Set the current conversation and load its messages */
  setCurrentConversation: (conversation: Conversation | null) => void;
  /** Add a user message */
  addUserMessage: (
    content: string,
    files?: ChatMessage["files"],
    historyMode?: HistoryMode
  ) => string;
  /** Add assistant messages (after streaming completes) */
  addAssistantMessages: (
    messages: Array<{
      model: string;
      /** Instance ID for multi-instance support. Falls back to model if not provided. */
      instanceId?: string;
      content: string;
      usage?: MessageUsage;
      modeMetadata?: MessageModeMetadata;
      error?: string;
      citations?: Citation[];
      artifacts?: Artifact[];
      toolExecutionRounds?: ToolExecutionRound[];
      completedRounds?: CompletedRound[];
      debugMessageId?: string;
    }>
  ) => void;
  /** Update a specific message */
  updateMessage: (messageId: string, updates: Partial<ChatMessage>) => void;
  /** Delete all messages after a given message ID (for edit-and-rerun) */
  deleteMessagesAfter: (messageId: string) => void;
  /** Replace an assistant message (for regeneration) */
  replaceAssistantMessage: (
    userMessageId: string,
    model: string,
    updates: Partial<ChatMessage>
  ) => void;
  /** Set feedback on a message */
  setMessageFeedback: (userMessageId: string, model: string, feedback: ResponseFeedback) => void;
  /** Set selected best response for a user message group */
  setSelectedBest: (userMessageId: string, model: string | null) => void;
  /** Clear all messages */
  clearMessages: () => void;
  /** Set messages directly (for loading from storage) */
  setMessages: (messages: ChatMessage[]) => void;
  /**
   * Set selected models.
   * @deprecated Use setSelectedInstances instead.
   */
  setSelectedModels: (models: string[]) => void;
  /** Set selected model instances */
  setSelectedInstances: (instances: ModelInstance[]) => void;
  /** Add a new instance for a model (allows duplicate models with different settings) */
  addInstance: (instance: ModelInstance) => void;
  /** Remove an instance by ID */
  removeInstance: (instanceId: string) => void;
  /** Update an existing instance */
  updateInstance: (instanceId: string, updates: Partial<Omit<ModelInstance, "id">>) => void;
  /** Create a new conversation */
  createConversation: (models: string[]) => Conversation;
  /** Update conversation in the list */
  updateConversationInList: (
    conversationId: string,
    messages: ChatMessage[],
    models: string[]
  ) => void;
}

export type ConversationStore = ConversationState & ConversationActions;

export const useConversationStore = create<ConversationStore>((set) => ({
  conversations: [],
  currentConversation: null,
  messages: [],
  selectedModels: [],
  selectedInstances: [],
  discardedUsage: emptyUsage(),
  discardedResponseCount: 0,

  setConversations: (conversations) => set({ conversations }),

  setCurrentConversation: (conversation) => {
    const models = conversation?.models ?? [];
    // Convert models to instances for backwards compatibility
    const instances = models.map((modelId) => createDefaultInstance(modelId));
    return set({
      currentConversation: conversation,
      messages: conversation?.messages ?? [],
      selectedModels: models,
      selectedInstances: instances,
      // Switching conversations starts a fresh session-only discard tally.
      discardedUsage: emptyUsage(),
      discardedResponseCount: 0,
    });
  },

  addUserMessage: (content, files, historyMode) => {
    const id = crypto.randomUUID();
    const message: ChatMessage = {
      id,
      role: "user",
      content,
      timestamp: new Date(),
      files,
      historyMode,
    };
    set((state) => ({
      messages: [...state.messages, message],
    }));
    return id;
  },

  addAssistantMessages: (newMessages) =>
    set((state) => ({
      messages: [
        ...state.messages,
        ...newMessages.map((m) => ({
          id: crypto.randomUUID(),
          role: "assistant" as const,
          content: m.content,
          model: m.model,
          // Use instanceId if provided, otherwise fall back to model for backwards compat
          instanceId: m.instanceId ?? m.model,
          timestamp: new Date(),
          usage: m.usage,
          modeMetadata: m.modeMetadata,
          error: m.error,
          citations: m.citations,
          artifacts: m.artifacts,
          toolExecutionRounds: m.toolExecutionRounds,
          completedRounds: m.completedRounds,
          debugMessageId: m.debugMessageId,
        })),
      ],
    })),

  updateMessage: (messageId, updates) =>
    set((state) => ({
      messages: state.messages.map((msg) => (msg.id === messageId ? { ...msg, ...updates } : msg)),
    })),

  deleteMessagesAfter: (messageId) =>
    set((state) => {
      const messageIndex = state.messages.findIndex((m) => m.id === messageId);
      if (messageIndex === -1) return state;
      // The dropped responses were really billed even though they leave the
      // conversation — fold their usage into the session discard tally so
      // "total spent" stays accurate after an edit-and-rerun.
      const removed = state.messages.slice(messageIndex + 1);
      const { discardedUsage, discardedResponseCount } = mergeDiscarded(state, removed);
      // Keep messages up to and including the specified message
      return {
        messages: state.messages.slice(0, messageIndex + 1),
        discardedUsage,
        discardedResponseCount,
      };
    }),

  replaceAssistantMessage: (userMessageId, model, updates) =>
    set((state) => {
      const messages = [...state.messages];
      const userIndex = messages.findIndex((m) => m.id === userMessageId);
      if (userIndex === -1) return state;

      let replaced = false;
      // Usage of the response being overwritten by regeneration — billed but
      // about to disappear, so it must be carried into the discard tally.
      let discardDelta: ChatMessage[] = [];
      for (let i = userIndex + 1; i < messages.length; i++) {
        if (messages[i].role === "user") break;
        if (messages[i].role === "assistant" && messages[i].model === model) {
          discardDelta = [messages[i]];
          messages[i] = {
            ...messages[i],
            ...updates,
            timestamp: new Date(),
          };
          replaced = true;
          break;
        }
      }

      // If no existing message found, insert a new one
      if (!replaced) {
        let insertIndex = userIndex + 1;
        while (insertIndex < messages.length && messages[insertIndex].role === "assistant") {
          insertIndex++;
        }
        messages.splice(insertIndex, 0, {
          id: crypto.randomUUID(),
          role: "assistant",
          content: updates.content ?? "",
          model,
          timestamp: new Date(),
          ...updates,
        });
      }

      const { discardedUsage, discardedResponseCount } = mergeDiscarded(state, discardDelta);
      return { messages, discardedUsage, discardedResponseCount };
    }),

  setMessageFeedback: (userMessageId, model, feedback) =>
    set((state) => {
      const messages = [...state.messages];
      const userIndex = messages.findIndex((m) => m.id === userMessageId);
      if (userIndex === -1) return state;

      for (let i = userIndex + 1; i < messages.length; i++) {
        if (messages[i].role === "user") break;
        if (messages[i].role === "assistant" && messages[i].model === model) {
          messages[i] = {
            ...messages[i],
            feedback: {
              rating: feedback,
              selectedAsBest: messages[i].feedback?.selectedAsBest,
            },
          };
          break;
        }
      }

      return { messages };
    }),

  setSelectedBest: (userMessageId, model) =>
    set((state) => {
      const messages = [...state.messages];
      const userIndex = messages.findIndex((m) => m.id === userMessageId);
      if (userIndex === -1) return state;

      // Update all assistant messages in this group
      for (let i = userIndex + 1; i < messages.length; i++) {
        if (messages[i].role === "user") break;
        if (messages[i].role === "assistant") {
          const isSelected = model !== null && messages[i].model === model;
          const currentFeedback: ResponseFeedbackData = messages[i].feedback ?? {
            rating: null,
          };
          messages[i] = {
            ...messages[i],
            feedback: {
              ...currentFeedback,
              selectedAsBest: model === null ? undefined : isSelected,
            },
          };
        }
      }

      return { messages };
    }),

  clearMessages: () =>
    set({ messages: [], discardedUsage: emptyUsage(), discardedResponseCount: 0 }),

  setMessages: (messages) =>
    set({ messages, discardedUsage: emptyUsage(), discardedResponseCount: 0 }),

  setSelectedModels: (models) => {
    // Also update instances for backwards compatibility
    const instances = models.map((modelId) => createDefaultInstance(modelId));
    return set({ selectedModels: models, selectedInstances: instances });
  },

  setSelectedInstances: (instances) => {
    // Also update selectedModels for backwards compatibility
    const models = instances.map((i) => i.modelId);
    return set({ selectedInstances: instances, selectedModels: models });
  },

  addInstance: (instance) =>
    set((state) => {
      const newInstances = [...state.selectedInstances, instance];
      const newModels = newInstances.map((i) => i.modelId);
      return { selectedInstances: newInstances, selectedModels: newModels };
    }),

  removeInstance: (instanceId) =>
    set((state) => {
      const newInstances = state.selectedInstances.filter((i) => i.id !== instanceId);
      const newModels = newInstances.map((i) => i.modelId);
      return { selectedInstances: newInstances, selectedModels: newModels };
    }),

  updateInstance: (instanceId, updates) =>
    set((state) => {
      const newInstances = state.selectedInstances.map((i) =>
        i.id === instanceId ? { ...i, ...updates } : i
      );
      const newModels = newInstances.map((i) => i.modelId);
      return { selectedInstances: newInstances, selectedModels: newModels };
    }),

  createConversation: (models) => {
    const conversation: Conversation = {
      id: crypto.randomUUID(),
      title: "New Chat",
      messages: [],
      models,
      createdAt: new Date(),
      updatedAt: new Date(),
    };
    const instances = models.map((modelId) => createDefaultInstance(modelId));
    set((state) => ({
      conversations: [conversation, ...state.conversations],
      currentConversation: conversation,
      messages: [],
      selectedModels: models,
      selectedInstances: instances,
    }));
    return conversation;
  },

  updateConversationInList: (conversationId, messages, models) =>
    set((state) => {
      const conversations = state.conversations.map((conv) => {
        if (conv.id !== conversationId) return conv;
        // Generate title from first user message if untitled
        let title = conv.title;
        if (title === "New Chat" && messages.length > 0) {
          const firstUserMsg = messages.find((m) => m.role === "user");
          if (firstUserMsg) {
            title =
              firstUserMsg.content.slice(0, 50) + (firstUserMsg.content.length > 50 ? "..." : "");
          }
        }
        return {
          ...conv,
          messages,
          models,
          title,
          updatedAt: new Date(),
        };
      });

      // Also update currentConversation if it matches
      const currentConversation =
        state.currentConversation?.id === conversationId
          ? (conversations.find((c) => c.id === conversationId) ?? null)
          : state.currentConversation;

      return { conversations, currentConversation };
    }),
}));

/**
 * Surgical Selectors - Prevent Unnecessary Re-renders
 *
 * Use these instead of accessing the store directly. Each selector subscribes
 * to the minimum data needed, preventing cascade re-renders.
 *
 * IMPORTANT: Avoid `useConversationStore(state => state)` - this subscribes
 * to the entire store and defeats the purpose of surgical subscriptions.
 */

/** Get messages only - use this in ChatMessageList */
export const useMessages = () => useConversationStore((state) => state.messages);

/** Get a specific message by ID - useful for editing individual messages */
export const useMessage = (messageId: string) =>
  useConversationStore((state) => state.messages.find((m) => m.id === messageId));

/**
 * Get selected models - used by ChatHeader and ChatInput.
 * @deprecated Prefer useSelectedInstances for multi-instance support.
 */
export const useSelectedModels = () => useConversationStore((state) => state.selectedModels);

/** Get selected model instances - use this for multi-instance support */
export const useSelectedInstances = () => useConversationStore((state) => state.selectedInstances);

/** Get a specific instance by ID */
export const useInstance = (instanceId: string) =>
  useConversationStore((state) => state.selectedInstances.find((i) => i.id === instanceId));

/**
 * Get current conversation metadata without messages.
 *
 * Use this when you only need id/title/timestamps - avoids re-render on message changes.
 */
export const useCurrentConversationMeta = () =>
  useConversationStore((state) =>
    state.currentConversation
      ? {
          id: state.currentConversation.id,
          title: state.currentConversation.title,
          createdAt: state.currentConversation.createdAt,
          updatedAt: state.currentConversation.updatedAt,
        }
      : null
  );

/** Get all conversations for the sidebar */
export const useConversations = () => useConversationStore((state) => state.conversations);

/**
 * Get current conversation for export.
 *
 * Returns a complete Conversation object with current messages and models.
 * Only use this when you need the full conversation (e.g., for export).
 * Prefer useCurrentConversationMeta for display-only needs.
 */
export const useCurrentConversationForExport = () =>
  useConversationStore((state) =>
    state.currentConversation
      ? {
          ...state.currentConversation,
          messages: state.messages,
          models: state.selectedModels,
        }
      : null
  );

/**
 * Check if there are any messages.
 *
 * Returns boolean only - avoids subscribing to messages array contents.
 * Use this for conditional rendering (e.g., show EmptyChat vs MessageList).
 */
export const useHasMessages = () => useConversationStore((state) => state.messages.length > 0);

/** Helper to add usage values into an accumulator */
function addUsage(acc: MessageUsage, usage: MessageUsage | undefined): void {
  if (!usage) return;
  acc.inputTokens += usage.inputTokens;
  acc.outputTokens += usage.outputTokens;
  acc.totalTokens += usage.totalTokens;
  if (usage.cost !== undefined) {
    acc.cost = (acc.cost ?? 0) + usage.cost;
  }
  if (usage.cachedTokens !== undefined) {
    acc.cachedTokens = (acc.cachedTokens ?? 0) + usage.cachedTokens;
  }
  if (usage.reasoningTokens !== undefined) {
    acc.reasoningTokens = (acc.reasoningTokens ?? 0) + usage.reasoningTokens;
  }
}

/** Create an empty usage object */
function emptyUsage(): MessageUsage {
  return {
    inputTokens: 0,
    outputTokens: 0,
    totalTokens: 0,
    cost: 0,
    cachedTokens: 0,
    reasoningTokens: 0,
  };
}

/**
 * Add a message's mode-specific overhead (router/synthesizer/vote/summary/
 * decomposition/aggregate sub-calls) into an accumulator. Shared by the live
 * usage tally and the discard tally so both count overhead identically.
 */
function addModeOverhead(acc: MessageUsage, meta: MessageModeMetadata | undefined): void {
  if (!meta) return;
  addUsage(acc, meta.routerUsage);
  addUsage(acc, meta.synthesizerUsage);
  addUsage(acc, meta.voteUsage);
  addUsage(acc, meta.summaryUsage);
  addUsage(acc, meta.decompositionUsage);
  addUsage(acc, meta.aggregateUsage);
}

/**
 * Fold the usage (response + mode overhead) of removed assistant messages into
 * a fresh copy of the running discard accumulator. Returns the next
 * `discardedUsage`/`discardedResponseCount` pair for a store update; the input
 * accumulator is never mutated.
 */
function mergeDiscarded(
  state: ConversationState,
  removed: ChatMessage[]
): { discardedUsage: MessageUsage; discardedResponseCount: number } {
  const discardedUsage = emptyUsage();
  addUsage(discardedUsage, state.discardedUsage);
  let added = 0;
  for (const msg of removed) {
    if (msg.role !== "assistant") continue;
    // Only count responses that carried billable data; placeholder/errored
    // turns with neither usage nor overhead don't move the tally.
    if (!msg.usage && !msg.modeMetadata) continue;
    addUsage(discardedUsage, msg.usage);
    addModeOverhead(discardedUsage, msg.modeMetadata);
    added++;
  }
  return {
    discardedUsage,
    discardedResponseCount: state.discardedResponseCount + added,
  };
}

/** Result from useTotalUsage including mode overhead breakdown */
export interface TotalUsageResult {
  /** Total usage from all message responses currently in the conversation */
  total: MessageUsage;
  /** Aggregate usage from mode-specific overhead (routing, synthesis, voting, etc.) */
  modeOverhead: MessageUsage;
  /**
   * Combined total + modeOverhead — the cost of the conversation **as it
   * currently stands** (i.e. what re-sending it now would weigh). This is the
   * "context" figure and excludes discarded responses and title generation.
   */
  grandTotal: MessageUsage;
  /**
   * Usage of responses no longer in the conversation — those discarded by
   * edit-and-rerun or overwritten by regeneration this session. Real spend
   * that `grandTotal` no longer reflects.
   */
  discarded: MessageUsage;
  /** Number of responses counted into `discarded`. */
  discardedResponseCount: number;
  /** Usage from automatic title generation, if any (lives on the conversation). */
  titleGeneration?: MessageUsage;
  /**
   * Everything actually spent on this conversation this session:
   * `grandTotal + discarded + titleGeneration`. This is the honest "total
   * spent" figure; it can exceed `grandTotal` after edits/regenerations.
   */
  spentTotal: MessageUsage;
}

/**
 * Calculate total usage across all messages, including mode-specific overhead.
 *
 * Mode overhead includes:
 * - routerUsage (routed mode)
 * - synthesizerUsage (synthesized mode)
 * - voteUsage (elected mode)
 * - summaryUsage (debated mode)
 * - decompositionUsage (hierarchical mode)
 * - aggregateUsage (consensus mode)
 *
 * Uses `useMemo` to only recompute when messages array changes.
 */
export const useTotalUsage = (): TotalUsageResult | null => {
  const messages = useMessages();
  const discarded = useConversationStore((state) => state.discardedUsage);
  const discardedResponseCount = useConversationStore((state) => state.discardedResponseCount);
  const titleGeneration = useConversationStore(
    (state) => state.currentConversation?.titleGenerationUsage
  );

  return useMemo(() => {
    const total = emptyUsage();
    const modeOverhead = emptyUsage();

    for (const msg of messages) {
      // Add regular message usage
      addUsage(total, msg.usage);
      // Add mode-specific overhead from modeMetadata
      addModeOverhead(modeOverhead, msg.modeMetadata);
    }

    // Calculate grand total (the "context" cost of the live conversation)
    const grandTotal = emptyUsage();
    addUsage(grandTotal, total);
    addUsage(grandTotal, modeOverhead);

    // Total spent this session = context + discarded responses + title gen.
    const spentTotal = emptyUsage();
    addUsage(spentTotal, grandTotal);
    addUsage(spentTotal, discarded);
    addUsage(spentTotal, titleGeneration);

    // Nothing has been spent yet anywhere — render nothing.
    if (spentTotal.totalTokens === 0) {
      return null;
    }

    return {
      total,
      modeOverhead,
      grandTotal,
      discarded,
      discardedResponseCount,
      titleGeneration,
      spentTotal,
    };
  }, [messages, discarded, discardedResponseCount, titleGeneration]);
};
