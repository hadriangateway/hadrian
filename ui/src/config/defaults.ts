import type { UiConfig, PagesConfig } from "./types";

const enabledPage = { status: "enabled" as const };

export const defaultPagesConfig: PagesConfig = {
  chat: enabledPage,
  studio: enabledPage,
  projects: enabledPage,
  teams: enabledPage,
  knowledge_bases: enabledPage,
  containers: enabledPage,
  api_keys: enabledPage,
  providers: enabledPage,
  templates: enabledPage,
  skills: enabledPage,
  usage: enabledPage,
  admin: {
    dashboard: enabledPage,
    organizations: enabledPage,
    projects: enabledPage,
    teams: enabledPage,
    service_accounts: enabledPage,
    users: enabledPage,
    sso: enabledPage,
    session_info: enabledPage,
    api_keys: enabledPage,
    providers: enabledPage,
    provider_health: enabledPage,
    knowledge_bases: enabledPage,
    pricing: enabledPage,
    usage: enabledPage,
    audit_logs: enabledPage,
    settings: enabledPage,
  },
};

export const defaultConfig: UiConfig = {
  branding: {
    title: "Hadrian Gateway",
    tagline: null,
    logo_url: null,
    logo_dark_url: null,
    favicon_url: null,
    colors: {},
    colors_dark: null,
    fonts: null,
    footer_text: null,
    footer_links: [],
    show_version: false,
    version: null,
    login: null,
  },
  chat: {
    enabled: true,
    default_model: null,
    available_models: [],
    file_uploads_enabled: true,
    max_file_size_bytes: 10 * 1024 * 1024, // 10MB
    allowed_file_types: [], // Empty = allow all filetypes
  },
  admin: {
    enabled: true,
  },
  auth: {
    methods: ["none"], // Default to no auth for easy development
    oidc: null,
  },
  sovereignty: {
    custom_fields: [],
  },
  pages: defaultPagesConfig,
  mcp: {
    favorites: [],
  },
};

export function getApiBaseUrl(): string {
  // In development, Vite proxy handles this
  // In production, use the same origin or env variable
  if (import.meta.env.VITE_API_URL) {
    return import.meta.env.VITE_API_URL;
  }
  return window.location.origin;
}
