export type Theme = "light" | "dark" | "system";

export type ConversationOwnerType = "user" | "project";

export type ModelTask = "chat" | "images" | "video" | "tts" | "transcription" | "translation";

export interface ConversationOwnerPreference {
  type: ConversationOwnerType;
  project_id?: string;
}

export interface UserPreferences {
  theme: Theme;
  defaultConversationOwner: ConversationOwnerPreference;
  sidebarCollapsed: boolean;
  /** Sidebar width in pixels (min 180, max 400, default 256) */
  sidebarWidth: number;
  /** Admin sidebar collapsed state (separate from main sidebar) */
  adminSidebarCollapsed: boolean;
  /** Admin sidebar width in pixels (min 180, max 400, default 256) */
  adminSidebarWidth: number;
  defaultModels: Partial<Record<ModelTask, string[]>>;
  favoriteModels: Partial<Record<ModelTask, string[]>>;
  showTokenCounts: boolean;
  showCosts: boolean;
  compactMessages: boolean;
  /** Model to use for auto-generating conversation titles. Empty string disables LLM title generation. */
  titleGenerationModel: string;
}

export const defaultPreferences: UserPreferences = {
  theme: "dark",
  defaultConversationOwner: { type: "user" },
  sidebarCollapsed: false,
  sidebarWidth: 256,
  adminSidebarCollapsed: false,
  adminSidebarWidth: 220,
  defaultModels: {},
  favoriteModels: {},
  showTokenCounts: true,
  showCosts: true,
  compactMessages: false,
  titleGenerationModel: "openai/gpt-5-nano",
};

export const SIDEBAR_MIN_WIDTH = 180;
export const SIDEBAR_MAX_WIDTH = 400;
export const SIDEBAR_DEFAULT_WIDTH = 256;
export const SIDEBAR_COLLAPSED_WIDTH = 64;
