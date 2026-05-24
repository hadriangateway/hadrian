import {
  AlertCircle,
  ArrowsUpFromLine,
  Clock,
  MousePointerClick,
  Paperclip,
  Send,
  Settings2,
  Split,
  StopCircle,
  X,
} from "lucide-react";
import { useCallback, useEffect, useMemo, useRef, useState, useSyncExternalStore } from "react";

// Detect if the primary pointer is coarse (touch device)
// Uses useSyncExternalStore for SSR safety and proper React integration
function subscribeToMediaQuery(callback: () => void) {
  const mql = window.matchMedia("(pointer: coarse)");
  mql.addEventListener("change", callback);
  return () => mql.removeEventListener("change", callback);
}

function getIsTouchDevice() {
  if (typeof window === "undefined") return false;
  return window.matchMedia("(pointer: coarse)").matches;
}

function useIsTouchDevice() {
  return useSyncExternalStore(subscribeToMediaQuery, getIsTouchDevice, () => false);
}

import type { VectorStoreOwnerType } from "@/api/generated/types.gen";
import { Button } from "@/components/Button/Button";
import { Textarea } from "@/components/Textarea/Textarea";
import { Tooltip, TooltipContent, TooltipTrigger } from "@/components/Tooltip/Tooltip";
import { TemplatesButton } from "@/components/PromptsButton";
import { SkillsButton } from "@/components/SkillsButton/SkillsButton";
import { SlashCommandPopover } from "@/components/ChatInput/SlashCommandPopover";
import { ToolsBar } from "@/components/ToolsBar";
import { useUserSkills } from "@/hooks/useUserSkills";
import type { Skill } from "@/api/generated/types.gen";
import { detectSlashQuery, matchSkills } from "@/pages/chat/utils/slashCommandMatcher";
import type { ModelInfo } from "@/components/ModelPicker/ModelPicker";
import { useConfig } from "@/config/ConfigProvider";
import { fileToBase64 } from "@/utils/fileToBase64";
import { formatBytes } from "@/utils/formatters";
import { cn } from "@/utils/cn";
import { isFileTypeAllowed, buildAcceptAttribute } from "@/utils/fileTypes";
import { useQuotedText, usePendingPrompt, useChatUIStore } from "@/stores/chatUIStore";

import type { ChatFile, HistoryMode, QueuedMessage } from "@/components/chat-types";
import {
  File,
  FileText,
  FileCode,
  FileAudio,
  FileVideo,
  FileSpreadsheet,
  FileArchive,
} from "lucide-react";

function getFileIcon(type: string, name: string) {
  if (type.startsWith("image/")) return File;
  if (type.startsWith("video/")) return FileVideo;
  if (type.startsWith("audio/")) return FileAudio;
  if (type.startsWith("text/")) return FileText;

  if (type === "application/pdf") return FileText;
  if (type === "application/json") return FileCode;
  if (type.includes("spreadsheet") || type.includes("excel") || type === "text/csv")
    return FileSpreadsheet;
  if (
    type.includes("zip") ||
    type.includes("tar") ||
    type.includes("archive") ||
    type.includes("compressed")
  )
    return FileArchive;
  if (
    type.includes("javascript") ||
    type.includes("typescript") ||
    type.includes("html") ||
    type.includes("css")
  )
    return FileCode;

  const ext = name.split(".").pop()?.toLowerCase();
  if (ext) {
    const codeExts = [
      "js",
      "ts",
      "jsx",
      "tsx",
      "py",
      "rs",
      "go",
      "java",
      "c",
      "cpp",
      "h",
      "rb",
      "php",
      "swift",
      "kt",
    ];
    const textExts = ["txt", "md", "rst", "log"];
    const spreadsheetExts = ["csv", "xlsx", "xls", "ods"];
    const archiveExts = ["zip", "tar", "gz", "rar", "7z", "bz2"];

    if (codeExts.includes(ext)) return FileCode;
    if (textExts.includes(ext)) return FileText;
    if (spreadsheetExts.includes(ext)) return FileSpreadsheet;
    if (archiveExts.includes(ext)) return FileArchive;
  }

  return File;
}

interface ChatInputProps {
  onSend: (content: string, files: ChatFile[]) => void;
  onStop?: () => void;
  onSettingsClick?: () => void;
  isStreaming?: boolean;
  disabled?: boolean;
  /** Whether no models are selected (shows a prominent hint overlay) */
  noModelsSelected?: boolean;
  /** Whether no models are available at all (e.g. no providers configured) */
  noModelsAvailable?: boolean;
  placeholder?: string;
  hasSystemPrompt?: boolean;
  /** Whether multiple models are selected (shows history mode toggle) */
  hasMultipleModels?: boolean;
  /** Current history mode setting */
  historyMode?: HistoryMode;
  /** Callback when history mode changes */
  onHistoryModeChange?: (mode: HistoryMode) => void;
  /** Enabled tool IDs */
  enabledTools?: string[];
  /** Callback when enabled tools change */
  onEnabledToolsChange?: (tools: string[]) => void;
  /** Attached vector store IDs (for file_search tool requirement check) */
  vectorStoreIds?: string[];
  /** Callback when vector store IDs change */
  onVectorStoreIdsChange?: (ids: string[]) => void;
  /** Owner type for vector store filtering */
  vectorStoreOwnerType?: VectorStoreOwnerType;
  /** Owner ID for vector store filtering */
  vectorStoreOwnerId?: string;
  /** Available models for sub-agent selection */
  availableModels?: ModelInfo[];
  /** Currently selected sub-agent model */
  subAgentModel?: string | null;
  /** Callback when sub-agent model changes */
  onSubAgentModelChange?: (model: string | null) => void;
  /** Callback to open MCP server configuration modal */
  onOpenMCPConfig?: () => void;
  /** Callback when a prompt template is applied */
  onApplyPrompt?: (content: string) => void;
  /** Messages queued while a response is streaming (sent as each turn completes) */
  queuedMessages?: QueuedMessage[];
  /** Remove a queued message before it is sent */
  onRemoveQueuedMessage?: (id: string) => void;
}

export function ChatInput({
  onSend,
  onStop,
  onSettingsClick,
  isStreaming = false,
  disabled = false,
  noModelsSelected = false,
  noModelsAvailable = false,
  placeholder = "Type a message...",
  hasSystemPrompt = false,
  hasMultipleModels = false,
  historyMode = "all",
  onHistoryModeChange,
  enabledTools = [],
  onEnabledToolsChange,
  vectorStoreIds,
  onVectorStoreIdsChange,
  vectorStoreOwnerType,
  vectorStoreOwnerId,
  availableModels,
  subAgentModel,
  onSubAgentModelChange,
  onOpenMCPConfig,
  onApplyPrompt,
  queuedMessages = [],
  onRemoveQueuedMessage,
}: ChatInputProps) {
  const [content, setContent] = useState("");
  const [files, setFiles] = useState<ChatFile[]>([]);
  const [isDragging, setIsDragging] = useState(false);
  const fileInputRef = useRef<HTMLInputElement>(null);
  const textareaRef = useRef<HTMLTextAreaElement>(null);
  const { config } = useConfig();
  const isTouchDevice = useIsTouchDevice();

  // Slash-command state. When the caret is inside a `/token` the popover
  // shows skill suggestions; Enter invokes the picked skill by prefixing
  // the submitted message with "Use the <name> skill for this request."
  // and letting the `Skill` tool handle the actual load.
  const { skills: userSkills } = useUserSkills();
  const [slashQuery, setSlashQuery] = useState<{
    query: string;
    start: number;
    end: number;
  } | null>(null);
  const [slashActiveIndex, setSlashActiveIndex] = useState(0);
  const [slashMatchCount, setSlashMatchCount] = useState(0);
  const [pendingSkill, setPendingSkill] = useState<Skill | null>(null);

  // Quote selection: insert quoted text as markdown blockquote
  const quotedText = useQuotedText();
  const clearQuotedText = useChatUIStore((s) => s.clearQuotedText);

  useEffect(() => {
    if (quotedText) {
      const blockquote = `> ${quotedText.text.replace(/\n/g, "\n> ")}\n\n`;
      setContent((prev) => blockquote + prev);
      clearQuotedText();
      // Focus textarea and move cursor to end
      setTimeout(() => {
        textareaRef.current?.focus();
      }, 0);
    }
  }, [quotedText, clearQuotedText]);

  // Pending prompt: replace content with example prompt
  const pendingPrompt = usePendingPrompt();
  const clearPendingPrompt = useChatUIStore((s) => s.clearPendingPrompt);

  useEffect(() => {
    if (pendingPrompt) {
      setContent(pendingPrompt);
      clearPendingPrompt();
      // Focus textarea and move cursor to end
      setTimeout(() => {
        textareaRef.current?.focus();
        // Move cursor to end of content
        if (textareaRef.current) {
          textareaRef.current.selectionStart = pendingPrompt.length;
          textareaRef.current.selectionEnd = pendingPrompt.length;
        }
      }, 0);
    }
  }, [pendingPrompt, clearPendingPrompt]);

  // Refocus textarea when streaming ends
  const wasStreamingRef = useRef(false);
  useEffect(() => {
    if (wasStreamingRef.current && !isStreaming) {
      textareaRef.current?.focus();
    }
    wasStreamingRef.current = isStreaming;
  }, [isStreaming]);

  const fileUploadsEnabled = config?.chat.file_uploads_enabled ?? false;
  const maxSize = config?.chat.max_file_size_bytes || 10 * 1024 * 1024;
  const allowedTypes = useMemo(
    () => config?.chat.allowed_file_types || [],
    [config?.chat.allowed_file_types]
  );

  const acceptAttribute = useMemo(() => buildAcceptAttribute(allowedTypes), [allowedTypes]);

  // Send always submits (queuing the message if a response is still streaming).
  // Stopping the current response is a separate button, shown while streaming.
  const handleSubmit = useCallback(() => {
    const trimmedContent = content.trim();
    if (!trimmedContent && files.length === 0) return;

    // If the user committed a slash-command for a skill, prepend a request
    // that tells the model to use it. The `Skill` tool handles the load.
    const finalContent = pendingSkill
      ? `Use the ${pendingSkill.name} skill for this request.\n\n${trimmedContent}`
      : trimmedContent;

    onSend(finalContent, files);
    setContent("");
    setFiles([]);
    setPendingSkill(null);
  }, [content, files, onSend, pendingSkill]);

  const enableSkill = useChatUIStore((s) => s.enableSkill);

  const commitSlashSkill = useCallback(
    (skill: Skill) => {
      setContent((prev) => {
        if (!slashQuery) return prev;
        // Strip the `/<query>` token; anything after the caret stays put.
        return prev.slice(0, slashQuery.start) + prev.slice(slashQuery.end);
      });
      // Enable the picked skill for this session so the `Skill` tool sees it
      // and the model can actually load it when asked.
      enableSkill(skill.id);
      setPendingSkill(skill);
      setSlashQuery(null);
      setSlashActiveIndex(0);
    },
    [slashQuery, enableSkill]
  );

  const handleKeyDown = useCallback(
    (e: React.KeyboardEvent) => {
      if (slashQuery && slashMatchCount > 0) {
        if (e.key === "ArrowDown") {
          e.preventDefault();
          setSlashActiveIndex((i) => Math.min(i + 1, slashMatchCount - 1));
          return;
        }
        if (e.key === "ArrowUp") {
          e.preventDefault();
          setSlashActiveIndex((i) => Math.max(i - 1, 0));
          return;
        }
        if (e.key === "Enter" && !e.shiftKey) {
          const matches = matchSkills(userSkills, slashQuery.query);
          const picked = matches[slashActiveIndex];
          if (picked) {
            e.preventDefault();
            commitSlashSkill(picked);
            return;
          }
        }
        if (e.key === "Escape") {
          e.preventDefault();
          setSlashQuery(null);
          return;
        }
        if (e.key === "Tab") {
          const matches = matchSkills(userSkills, slashQuery.query);
          const picked = matches[slashActiveIndex];
          if (picked) {
            e.preventDefault();
            commitSlashSkill(picked);
            return;
          }
        }
      }

      // On touch devices, let Enter add newlines naturally - users tap Send button
      // On desktop, Enter sends and Shift+Enter adds newline
      if (e.key === "Enter" && !e.shiftKey && !isTouchDevice) {
        e.preventDefault();
        handleSubmit();
      }
    },
    [
      handleSubmit,
      isTouchDevice,
      slashQuery,
      slashMatchCount,
      slashActiveIndex,
      userSkills,
      commitSlashSkill,
    ]
  );

  const updateSlashState = useCallback(
    (text: string, caret: number) => {
      const detected = detectSlashQuery(text, caret);
      if (!detected) {
        if (slashQuery !== null) {
          setSlashQuery(null);
          setSlashActiveIndex(0);
        }
        return;
      }
      setSlashQuery(detected);
      // Clamp activeIndex when the match set shrinks on each keystroke.
      const matches = matchSkills(userSkills, detected.query);
      setSlashActiveIndex((i) => Math.min(i, Math.max(matches.length - 1, 0)));
    },
    [slashQuery, userSkills]
  );

  const handleFileSelect = useCallback(
    async (selectedFiles: FileList | null) => {
      if (!selectedFiles) return;

      const newFiles: ChatFile[] = [];

      for (const file of Array.from(selectedFiles)) {
        try {
          if (file.size > maxSize) {
            console.warn(`File ${file.name} exceeds max size of ${maxSize} bytes`);
            continue;
          }

          if (!isFileTypeAllowed(file, allowedTypes)) {
            console.warn(
              `File ${file.name} type not allowed. MIME: "${file.type}", allowedTypes:`,
              allowedTypes
            );
            continue;
          }

          const base64 = await fileToBase64(file);
          const chatFile: ChatFile = {
            id: crypto.randomUUID(),
            name: file.name,
            type: file.type || "application/octet-stream", // Ensure type is never empty
            size: file.size,
            base64,
          };

          if (file.type.startsWith("image/")) {
            // base64 is already a full data URL from fileToBase64 (e.g., "data:image/png;base64,...")
            chatFile.preview = base64;
          }

          newFiles.push(chatFile);
        } catch (err) {
          console.error(`Error processing file ${file.name}:`, err);
        }
      }

      setFiles((prev) => [...prev, ...newFiles]);

      if (fileInputRef.current) {
        fileInputRef.current.value = "";
      }
    },
    [maxSize, allowedTypes]
  );

  const handleRemoveFile = useCallback((fileId: string) => {
    setFiles((prev) => prev.filter((f) => f.id !== fileId));
  }, []);

  const handleDrop = useCallback(
    (event: React.DragEvent) => {
      event.preventDefault();
      setIsDragging(false);
      if (event.dataTransfer.files.length > 0) {
        handleFileSelect(event.dataTransfer.files);
      }
    },
    [handleFileSelect]
  );

  const handlePaste = useCallback(
    (event: React.ClipboardEvent<HTMLTextAreaElement>) => {
      const pastedFiles = event.clipboardData?.files;
      if (pastedFiles && pastedFiles.length > 0) {
        // Prevent the textarea from inserting an image filename or data URL.
        event.preventDefault();
        handleFileSelect(pastedFiles);
      }
    },
    [handleFileSelect]
  );

  const handleDragOver = useCallback((event: React.DragEvent) => {
    event.preventDefault();
    setIsDragging(true);
  }, []);

  const handleDragLeave = useCallback((event: React.DragEvent) => {
    event.preventDefault();
    setIsDragging(false);
  }, []);

  const canSend = content.trim() || files.length > 0;

  return (
    <div className="space-y-2">
      {/* Queued messages: composed while a response was still streaming. They
          send one at a time as each in-flight turn completes. */}
      {queuedMessages.length > 0 && (
        <ul className="flex flex-col gap-1.5 px-1" aria-label="Queued messages">
          {queuedMessages.map((msg) => (
            <li
              key={msg.id}
              className="flex items-center gap-2 rounded-lg border border-dashed bg-muted/40 px-2.5 py-1.5 text-sm"
            >
              <Clock className="h-3.5 w-3.5 shrink-0 text-muted-foreground" />
              <span className="min-w-0 flex-1 truncate text-muted-foreground">
                {msg.content || (msg.files.length > 0 ? "(attachment only)" : "")}
              </span>
              {msg.files.length > 0 && (
                <span className="shrink-0 text-[10px] text-muted-foreground">
                  {msg.files.length} file{msg.files.length > 1 ? "s" : ""}
                </span>
              )}
              {onRemoveQueuedMessage && (
                <button
                  type="button"
                  onClick={() => onRemoveQueuedMessage(msg.id)}
                  className="shrink-0 rounded-full p-0.5 text-muted-foreground transition-colors hover:bg-muted hover:text-foreground"
                  aria-label={`Remove queued message: ${msg.content || "(attachment only)"}`}
                >
                  <X className="h-3.5 w-3.5" />
                </button>
              )}
            </li>
          ))}
        </ul>
      )}

      {/* File previews */}
      {files.length > 0 && (
        <div className="flex flex-wrap gap-2 px-1">
          {files.map((file) => (
            <div
              key={file.id}
              className="group relative flex items-center gap-2 rounded-lg border bg-card px-2.5 py-1.5 shadow-sm"
            >
              {file.preview ? (
                <img src={file.preview} alt={file.name} className="h-8 w-8 rounded object-cover" />
              ) : (
                (() => {
                  const Icon = getFileIcon(file.type, file.name);
                  return (
                    <div className="flex h-8 w-8 items-center justify-center rounded bg-muted">
                      <Icon className="h-4 w-4 text-muted-foreground" />
                    </div>
                  );
                })()
              )}
              <div className="flex flex-col">
                <span className="max-w-[100px] truncate text-xs font-medium">{file.name}</span>
                <span className="text-[10px] text-muted-foreground">{formatBytes(file.size)}</span>
              </div>
              <button
                onClick={() => handleRemoveFile(file.id)}
                className="ml-1 rounded-full p-0.5 text-muted-foreground hover:bg-muted hover:text-foreground transition-colors"
                aria-label={`Remove file: ${file.name}`}
              >
                <X className="h-3.5 w-3.5" />
              </button>
            </div>
          ))}
        </div>
      )}

      {/* Main input container */}
      <div
        className={cn(
          "relative rounded-2xl border bg-card shadow-sm transition-all",
          noModelsSelected && "border-dashed border-muted-foreground/30",
          isDragging && "border-primary ring-2 ring-primary/20",
          !noModelsSelected && "focus-within:shadow-md focus-within:border-primary/50"
        )}
        onDrop={fileUploadsEnabled ? handleDrop : undefined}
        onDragOver={fileUploadsEnabled ? handleDragOver : undefined}
        onDragLeave={fileUploadsEnabled ? handleDragLeave : undefined}
      >
        {/* No model selected overlay */}
        {noModelsSelected && (
          <div className="absolute inset-0 z-10 flex items-center justify-center rounded-2xl bg-card/80 backdrop-blur-[1px]">
            <div className="flex items-center gap-2 text-sm text-muted-foreground">
              {noModelsAvailable ? (
                <>
                  <AlertCircle className="h-4 w-4" />
                  <span>No models available. Add a provider in settings to get started.</span>
                </>
              ) : (
                <>
                  <MousePointerClick className="h-4 w-4" />
                  <span>Select a model above to start chatting</span>
                </>
              )}
            </div>
          </div>
        )}

        {/* Text input area */}
        <div className="relative">
          {pendingSkill && (
            <div className="mx-4 mt-2 inline-flex items-center gap-1 rounded-md bg-primary/10 px-2 py-0.5 text-xs font-mono text-primary">
              /{pendingSkill.name}
              <button
                type="button"
                className="ml-1 text-primary/70 hover:text-primary"
                aria-label="Clear pending skill"
                onClick={() => setPendingSkill(null)}
              >
                ×
              </button>
            </div>
          )}
          <Textarea
            ref={textareaRef}
            value={content}
            onChange={(e) => {
              setContent(e.target.value);
              updateSlashState(e.target.value, e.target.selectionStart ?? 0);
            }}
            onKeyUp={(e) => {
              const target = e.currentTarget;
              updateSlashState(target.value, target.selectionStart ?? 0);
            }}
            onClick={(e) => {
              const target = e.currentTarget;
              updateSlashState(target.value, target.selectionStart ?? 0);
            }}
            onKeyDown={handleKeyDown}
            onPaste={handlePaste}
            placeholder={placeholder}
            className="min-h-[56px] w-full resize-none border-0 bg-transparent px-4 pt-3 pb-1 text-base focus-visible:ring-0 focus-visible:ring-offset-0"
            autoResize
            maxHeight={200}
            disabled={disabled}
          />
          {slashQuery && (
            <SlashCommandPopover
              skills={userSkills}
              query={slashQuery.query}
              activeIndex={slashActiveIndex}
              onSelect={commitSlashSkill}
              onMatchesChange={(matches) => setSlashMatchCount(matches.length)}
            />
          )}
        </div>

        {/* Bottom toolbar */}
        <div className="flex items-center justify-between gap-2 px-2 pb-2">
          <div className="flex items-center gap-1 min-w-0 overflow-hidden">
            {/* Settings button */}
            {onSettingsClick && (
              <Tooltip>
                <TooltipTrigger asChild>
                  <Button
                    type="button"
                    size="icon"
                    variant="ghost"
                    className={cn(
                      "h-8 w-8 shrink-0 rounded-lg",
                      hasSystemPrompt
                        ? "text-primary"
                        : "text-muted-foreground hover:text-foreground"
                    )}
                    onClick={onSettingsClick}
                    disabled={disabled}
                    aria-label="Conversation settings"
                  >
                    <Settings2 className="h-4 w-4" />
                  </Button>
                </TooltipTrigger>
                <TooltipContent side="top">
                  <p>Conversation settings</p>
                </TooltipContent>
              </Tooltip>
            )}

            {/* Templates */}
            {onApplyPrompt && (
              <TemplatesButton onApplyTemplate={onApplyPrompt} disabled={disabled} />
            )}

            {/* Skills */}
            <SkillsButton disabled={disabled} />

            {/* History mode toggle - only show when multiple models */}
            {hasMultipleModels && onHistoryModeChange && (
              <Tooltip>
                <TooltipTrigger asChild>
                  <Button
                    type="button"
                    size="icon"
                    variant="ghost"
                    className={cn(
                      "h-8 w-8 shrink-0 rounded-lg",
                      historyMode === "same-model"
                        ? "text-primary"
                        : "text-muted-foreground hover:text-foreground"
                    )}
                    onClick={() =>
                      onHistoryModeChange(historyMode === "all" ? "same-model" : "all")
                    }
                    disabled={disabled}
                    aria-label={
                      historyMode === "all"
                        ? "Switch to isolated history"
                        : "Switch to shared history"
                    }
                  >
                    {historyMode === "all" ? (
                      <Split className="h-4 w-4" />
                    ) : (
                      <ArrowsUpFromLine className="h-4 w-4" />
                    )}
                  </Button>
                </TooltipTrigger>
                <TooltipContent side="top" className="max-w-[220px]">
                  <p className="font-medium">
                    {historyMode === "all" ? "Shared history" : "Isolated history"}
                  </p>
                  <p className="text-xs text-muted-foreground mt-0.5">
                    {historyMode === "all"
                      ? "All models see each other's responses."
                      : "Each model sees only its own responses."}
                  </p>
                  <p className="text-xs text-muted-foreground mt-1 border-t border-border pt-1">
                    {historyMode === "all"
                      ? "Click to isolate histories"
                      : "Click to share all responses"}
                  </p>
                </TooltipContent>
              </Tooltip>
            )}

            {/* Attach button */}
            {fileUploadsEnabled && (
              <>
                <input
                  ref={fileInputRef}
                  type="file"
                  multiple
                  accept={acceptAttribute}
                  onChange={(e) => handleFileSelect(e.target.files)}
                  className="hidden"
                  aria-label="Attach files"
                />
                <Tooltip>
                  <TooltipTrigger asChild>
                    <Button
                      type="button"
                      size="icon"
                      variant="ghost"
                      className="h-8 w-8 shrink-0 rounded-lg text-muted-foreground hover:text-foreground"
                      onClick={() => fileInputRef.current?.click()}
                      disabled={disabled}
                      aria-label="Attach files"
                    >
                      <Paperclip className="h-4 w-4" />
                    </Button>
                  </TooltipTrigger>
                  <TooltipContent side="top">
                    <p>Attach files (max {formatBytes(maxSize)})</p>
                  </TooltipContent>
                </Tooltip>
              </>
            )}

            {/* Tools bar */}
            {onEnabledToolsChange && (
              <ToolsBar
                enabledTools={enabledTools}
                onEnabledToolsChange={onEnabledToolsChange}
                vectorStoreIds={vectorStoreIds}
                onVectorStoreIdsChange={onVectorStoreIdsChange}
                vectorStoreOwnerType={vectorStoreOwnerType}
                vectorStoreOwnerId={vectorStoreOwnerId}
                disabled={disabled}
                availableModels={availableModels}
                subAgentModel={subAgentModel}
                onSubAgentModelChange={onSubAgentModelChange}
                onOpenMCPConfig={onOpenMCPConfig}
              />
            )}
          </div>

          {/* Send / Stop buttons. While streaming, Send queues the message and
              a separate Stop button aborts the in-flight response. */}
          <div className="flex shrink-0 items-center gap-1.5">
            {isStreaming && onStop && (
              <Button
                size="sm"
                variant="danger"
                className="h-8 gap-1.5 rounded-xl px-3 transition-all"
                onClick={onStop}
                aria-label="Stop response"
              >
                <StopCircle className="h-4 w-4" />
                Stop
              </Button>
            )}
            <Button
              size="sm"
              className="h-8 gap-1.5 rounded-xl px-3 transition-all"
              onClick={handleSubmit}
              disabled={disabled || !canSend}
              variant="primary"
              aria-label={isStreaming ? "Queue message" : "Send message"}
            >
              {isStreaming ? "Queue" : "Send"}
              <Send className="h-3.5 w-3.5" />
            </Button>
          </div>
        </div>
      </div>
    </div>
  );
}
