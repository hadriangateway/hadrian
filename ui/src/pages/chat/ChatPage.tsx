import { useEffect, useRef, useCallback, useMemo, useState } from "react";
import { useNavigate, useParams } from "react-router-dom";
import { useQuery } from "@tanstack/react-query";

import { apiV1ModelsOptions } from "@/api/generated/@tanstack/react-query.gen";
import { ChatView, type ChatFile } from "@/components/ChatView/ChatView";
import { useMessageQueue } from "./useMessageQueue";
import { ErrorBoundary } from "@/components/ErrorBoundary/ErrorBoundary";
import { useConversationsContext } from "@/components/ConversationsProvider/ConversationsProvider";
import {
  ForkConversationModal,
  type ForkConversationResult,
} from "@/components/ForkConversationModal/ForkConversationModal";
import type { ModelInfo } from "@/components/ModelSelector/ModelSelector";
import { useConversationSync } from "@/hooks/useConversationSync";
import { usePreferences } from "@/preferences/PreferencesProvider";
import { useConversationStore, useSelectedModels, useMessages } from "@/stores/conversationStore";
import {
  useSystemPrompt,
  useDisabledModels,
  useHistoryMode,
  useConversationMode,
  useModeConfig,
  usePerModelSettings,
  useVectorStoreIds,
  useClientSideRAG,
  useEnabledTools,
  useEnabledSkillIds,
  useDataFiles,
  useMaxToolIterations,
  useCaptureRawSSEEvents,
  useSubAgentModel,
  useAgentMemoryLimit,
  useAgentExpiresAfterMinutes,
  useAgentAllowedDomains,
  useToolSearchEnabled,
  useToolSearchRanker,
} from "@/stores/chatUIStore";
import { useUserSkills } from "@/hooks/useUserSkills";
import { useWasmSetup } from "@/components/WasmSetup/WasmSetupGuard";
import { setSkillCatalog } from "./utils/skillCache";

import type { ModelSettings } from "./types";
import { useChat } from "./useChat";

export default function ChatPage() {
  const { conversationId } = useParams();
  const navigate = useNavigate();
  const { preferences } = usePreferences();

  // Fetch models from API
  const { data: modelsResponse, isPending: isLoadingModels } = useQuery(apiV1ModelsOptions());
  const availableModels: ModelInfo[] = useMemo(
    () => modelsResponse?.data?.map((m) => m as ModelInfo).filter((m) => m.id) || [],
    [modelsResponse?.data]
  );

  // Use stores directly - they are the source of truth
  const selectedModels = useSelectedModels();
  const disabledModels = useDisabledModels();
  const messages = useMessages();
  const systemPrompt = useSystemPrompt();
  const historyMode = useHistoryMode();
  const conversationMode = useConversationMode();
  const modeConfig = useModeConfig();
  const perModelSettings = usePerModelSettings();
  const vectorStoreIds = useVectorStoreIds();
  const clientSideRAG = useClientSideRAG();
  const enabledTools = useEnabledTools();
  const enabledSkillIds = useEnabledSkillIds();
  const { skills: userSkills } = useUserSkills();
  const dataFiles = useDataFiles();
  const maxToolIterations = useMaxToolIterations();
  const captureRawSSEEvents = useCaptureRawSSEEvents();
  const subAgentModel = useSubAgentModel();
  const agentMemoryLimit = useAgentMemoryLimit();
  const agentExpiresAfterMinutes = useAgentExpiresAfterMinutes();
  const agentAllowedDomains = useAgentAllowedDomains();
  const toolSearchEnabled = useToolSearchEnabled();
  const toolSearchRanker = useToolSearchRanker();
  // The shell tool runs in a server-side container, which the zero-backend WASM
  // build cannot provide — never attach it there.
  const { isWasm } = useWasmSetup();

  const agentConfig = useMemo(
    () => ({
      // The shell tool is enabled via the `agent` entry in the tools bar.
      enabled: !isWasm && enabledTools.includes("agent"),
      memoryLimit: agentMemoryLimit,
      expiresAfterMinutes: agentExpiresAfterMinutes,
      allowedDomains: agentAllowedDomains,
      toolSearch: toolSearchEnabled,
      toolSearchRanker,
    }),
    [
      isWasm,
      enabledTools,
      agentMemoryLimit,
      agentExpiresAfterMinutes,
      agentAllowedDomains,
      toolSearchEnabled,
      toolSearchRanker,
    ]
  );

  const { setSelectedModels } = useConversationStore();

  // Track whether we've initialized default models from preferences
  const hasInitializedModelsRef = useRef(false);

  // Active models are selected models that aren't disabled
  const activeModels = useMemo(
    () => selectedModels.filter((m) => !disabledModels.includes(m)),
    [selectedModels, disabledModels]
  );

  // Build settings for useChat - only include systemPrompt
  // Per-model parameters come from perModelSettings
  const modelSettings: ModelSettings = useMemo(
    () => ({
      systemPrompt: systemPrompt || undefined,
    }),
    [systemPrompt]
  );

  // Keep the tool-side skill cache in sync with what the user can see.
  // The `Skill` tool executor looks up skills from this cache at call time.
  useEffect(() => {
    setSkillCatalog(userSkills);
  }, [userSkills]);

  // Resolve enabled skill IDs to full objects for the tools-array builder in
  // useChat. Only model-invocable + user-invocable skills go to the model.
  const enabledSkills = useMemo(
    () => userSkills.filter((s) => enabledSkillIds.includes(s.id)),
    [userSkills, enabledSkillIds]
  );

  // Set default models from preferences when models load (only once on initial load)
  useEffect(() => {
    if (hasInitializedModelsRef.current || availableModels.length === 0) return;

    hasInitializedModelsRef.current = true;
    const defaultModels = preferences.defaultModels?.chat || [];
    const validDefaults = defaultModels.filter((m) => availableModels.some((am) => am.id === m));
    if (validDefaults.length > 0) {
      setSelectedModels(validDefaults);
    }
  }, [availableModels, preferences.defaultModels, setSelectedModels]);

  // Enable client-side tool execution when:
  // 1. clientSideRAG is enabled (for client-side file_search)
  // 2. file_search is enabled (executes against vector stores)
  // 3. code_interpreter is enabled (it's a client-side only tool)
  // 4. js_code_interpreter is enabled (it's a client-side only tool)
  // 5. sql_query is enabled (it's a client-side only tool)
  // 6. chart_render is enabled (it's a client-side only tool)
  // 7. html_render is enabled (it's a client-side only tool)
  // 8. sub_agent is enabled (it's a client-side only tool)
  // 9. mcp is enabled (MCP tools are executed client-side)
  // 10. wikipedia is enabled (it's a client-side only tool)
  // 11. wikidata is enabled (it's a client-side only tool)
  // 12. web_search is enabled (backend-proxied tool)
  // 13. web_fetch is enabled (backend-proxied tool)
  const clientSideToolExecution =
    clientSideRAG ||
    enabledSkillIds.length > 0 ||
    enabledTools.includes("file_search") ||
    enabledTools.includes("code_interpreter") ||
    enabledTools.includes("js_code_interpreter") ||
    enabledTools.includes("sql_query") ||
    enabledTools.includes("chart_render") ||
    enabledTools.includes("html_render") ||
    enabledTools.includes("sub_agent") ||
    enabledTools.includes("mcp") ||
    enabledTools.includes("wikipedia") ||
    enabledTools.includes("wikidata") ||
    enabledTools.includes("web_search") ||
    enabledTools.includes("web_fetch");

  // Pass only active (non-disabled) models to useChat
  // Filter to only registered data files for SQL context
  const registeredDataFiles = useMemo(
    () =>
      dataFiles
        .filter((f) => f.registered)
        .map((f) => ({
          name: f.name,
          columns: f.columns,
          tables: f.tables,
          dbName: f.dbName,
        })),
    [dataFiles]
  );

  // Pending project selection for new conversations (before first message)
  const [pendingProject, setPendingProject] = useState<{
    id: string | null;
    name?: string;
  }>({ id: null });

  // Sync conversation state between persistence layer and stores
  // (must come before useChat so projectId is available)
  const { currentConversation, createConversation, forkConversation } =
    useConversationSync(conversationId);

  const {
    isStreaming,
    sendMessage,
    stopStreaming,
    clearMessages,
    regenerateResponse,
    editAndRerun,
    respondToMcpApproval,
  } = useChat({
    models: activeModels,
    settings: modelSettings,
    historyMode,
    conversationMode,
    modeConfig,
    perModelSettings,
    vectorStoreIds: vectorStoreIds.length > 0 ? vectorStoreIds : undefined,
    clientSideToolExecution,
    enabledTools,
    agentConfig,
    enabledSkills,
    dataFiles: registeredDataFiles.length > 0 ? registeredDataFiles : undefined,
    maxToolIterations,
    captureRawSSEEvents,
    subAgentModel,
    projectId: currentConversation?.projectId ?? pendingProject.id ?? undefined,
    // Use the stable local conversation id, not the URL param. After background
    // sync assigns a remoteId, useConversationSync rewrites the URL from the
    // local UUID to the remoteId — that URL flip would otherwise look like a
    // conversation switch to useChat and abort the in-flight stream.
    conversationId: currentConversation?.id ?? conversationId,
  });

  const { moveToProject } = useConversationsContext();

  const handleProjectChange = useCallback(
    (projectId: string | null, projectName?: string) => {
      if (!currentConversation) return;
      moveToProject(currentConversation.id, projectId, projectName);
    },
    [currentConversation, moveToProject]
  );

  const handlePendingProjectChange = useCallback(
    (projectId: string | null, projectName?: string) => {
      setPendingProject({ id: projectId, name: projectName });
    },
    []
  );

  // Fork modal state
  const [forkModalOpen, setForkModalOpen] = useState(false);
  const [forkMessageId, setForkMessageId] = useState<string | undefined>(undefined);

  // Message queue: lets the user keep composing (and hit "send") while a
  // response is still streaming. An idle send goes out immediately; a send
  // issued mid-turn is queued and dispatched when the turn completes. See
  // `MessageQueue` for why serialization keys off the `sendMessage` promise
  // rather than `isStreaming` (which flickers between tool rounds).
  const { queuedMessages, sendOrQueue, removeQueuedMessage, clearQueue } =
    useMessageQueue(sendMessage);

  // Drop pending queued messages when leaving the conversation they were queued
  // for, so the singleton doesn't drain them through a different conversation's
  // send context. The cleanup fires on a same-route switch (/chat/:idA →
  // /chat/:idB, where the id dep changes) and on a remount that unmounts this
  // instance (/chat/:id → /chat via "New Chat"), using the id captured at setup.
  // The create transition (/chat → /chat/:id) unmounts the conversation-less
  // /chat instance, whose captured id is undefined, so a queued follow-up is
  // preserved. The local→remote URL flip keeps currentConversation.id stable, so
  // the dep doesn't change and the queue survives it.
  useEffect(() => {
    const id = currentConversation?.id;
    return () => {
      if (id !== undefined) clearQueue();
    };
  }, [currentConversation?.id, clearQueue]);

  const handleSendMessage = useCallback(
    (content: string, files?: ChatFile[]) => {
      if (!currentConversation) {
        const newConv = createConversation(
          selectedModels,
          pendingProject.id ?? undefined,
          pendingProject.name
        );
        navigate(`/chat/${newConv.id}`, { replace: true });
        setPendingProject({ id: null });
      }
      sendOrQueue(content, files ?? []);
    },
    [currentConversation, createConversation, navigate, selectedModels, pendingProject, sendOrQueue]
  );

  // Handle regeneration of a single model response
  const handleRegenerate = useCallback(
    (userMessageId: string, model: string) => {
      regenerateResponse(userMessageId, model);
    },
    [regenerateResponse]
  );

  // Handle regeneration of all responses for a user message
  const handleRegenerateAll = useCallback(
    (messageId: string) => {
      const message = messages.find((m) => m.id === messageId);
      if (message && message.role === "user") {
        // Re-run with the same content (this deletes subsequent messages and re-queries all models)
        editAndRerun(messageId, message.content);
      }
    },
    [messages, editAndRerun]
  );

  // Handle forking conversation from a specific message - opens modal
  const handleForkFromMessage = useCallback(
    (messageId: string) => {
      if (!currentConversation) return;
      setForkMessageId(messageId);
      setForkModalOpen(true);
    },
    [currentConversation]
  );

  // Handle forking the entire current conversation - opens modal
  const handleForkConversation = useCallback(() => {
    if (!currentConversation) return;
    setForkMessageId(undefined);
    setForkModalOpen(true);
  }, [currentConversation]);

  // Handle the actual fork when modal confirms
  const handleForkConfirm = useCallback(
    (result: ForkConversationResult) => {
      if (!currentConversation) return;
      const forked = forkConversation(currentConversation.id, {
        upToMessageId: forkMessageId,
        newTitle: result.title,
        models: result.models,
        projectId: result.projectId,
        projectName: result.projectName,
      });
      navigate(`/chat/${forked.id}`);
    },
    [currentConversation, forkConversation, forkMessageId, navigate]
  );

  return (
    <>
      {/*
        Wrap the chat tree in an ErrorBoundary so a render-time crash inside
        any descendant — message list, model card, artifact renderer — falls
        back to a recoverable card instead of unmounting the whole shell. The
        boundary covers ChatMessageList, MultiModelResponse, ChatMessage,
        artifacts, etc. by virtue of sitting at the root of ChatView.
      */}
      <ErrorBoundary>
        <ChatView
          availableModels={availableModels}
          conversation={currentConversation}
          isStreaming={isStreaming}
          isLoadingModels={isLoadingModels}
          onSendMessage={handleSendMessage}
          onStopStreaming={stopStreaming}
          onClearMessages={clearMessages}
          onRegenerate={handleRegenerate}
          onRegenerateAll={handleRegenerateAll}
          onForkFromMessage={handleForkFromMessage}
          onFork={handleForkConversation}
          onProjectChange={handleProjectChange}
          onPendingProjectChange={!currentConversation ? handlePendingProjectChange : undefined}
          pendingProjectName={pendingProject.name}
          pendingProjectId={pendingProject.id}
          onEditAndRerun={editAndRerun}
          onRespondMcpApproval={respondToMcpApproval}
          queuedMessages={queuedMessages}
          onRemoveQueuedMessage={removeQueuedMessage}
        />
      </ErrorBoundary>
      {currentConversation && (
        <ForkConversationModal
          open={forkModalOpen}
          onClose={() => setForkModalOpen(false)}
          conversation={currentConversation}
          upToMessageId={forkMessageId}
          onFork={handleForkConfirm}
        />
      )}
    </>
  );
}
