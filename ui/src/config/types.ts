export type PageStatus = "enabled" | "disabled" | "notice";

export interface PageConfig {
  status: PageStatus;
  notice_message?: string;
}

export interface PagesConfig {
  chat: PageConfig;
  studio: PageConfig;
  projects: PageConfig;
  teams: PageConfig;
  knowledge_bases: PageConfig;
  containers: PageConfig;
  api_keys: PageConfig;
  providers: PageConfig;
  templates: PageConfig;
  skills: PageConfig;
  usage: PageConfig;
  admin: AdminPagesConfig;
}

export interface AdminPagesConfig {
  dashboard: PageConfig;
  organizations: PageConfig;
  projects: PageConfig;
  teams: PageConfig;
  service_accounts: PageConfig;
  users: PageConfig;
  sso: PageConfig;
  session_info: PageConfig;
  api_keys: PageConfig;
  providers: PageConfig;
  provider_health: PageConfig;
  knowledge_bases: PageConfig;
  pricing: PageConfig;
  usage: PageConfig;
  audit_logs: PageConfig;
  settings: PageConfig;
}

export interface UiConfig {
  branding: BrandingConfig;
  chat: ChatConfig;
  admin: AdminConfig;
  auth: AuthConfig;
  sovereignty: SovereigntyUiConfig;
  pages: PagesConfig;
  mcp: McpUiConfig;
}

export interface McpUiConfig {
  favorites: FavoriteMcpServer[];
}

export interface FavoriteMcpServer {
  name: string;
  /**
   * Either a direct remote URL (http(s)://...) the UI connects to, or a
   * registry identifier (e.g. `io.github.hadriangateway/platter`) that the UI
   * resolves against the public MCP registry.
   */
  url: string;
}

export interface BrandingConfig {
  title: string;
  tagline: string | null;
  logo_url: string | null;
  logo_dark_url: string | null;
  favicon_url: string | null;
  colors: ColorPalette;
  colors_dark: ColorPalette | null;
  fonts: FontsConfig | null;
  footer_text: string | null;
  footer_links: FooterLink[];
  show_version: boolean;
  version: string | null;
  login: LoginConfig | null;
}

export interface LoginConfig {
  title?: string;
  subtitle?: string;
  background_image?: string;
  show_logo: boolean;
}

export interface ColorPalette {
  primary?: string;
  primary_foreground?: string;
  secondary?: string;
  secondary_foreground?: string;
  accent?: string;
  background?: string;
  foreground?: string;
  muted?: string;
  border?: string;
}

export interface FooterLink {
  label: string;
  url: string;
}

export interface FontsConfig {
  heading?: string;
  body?: string;
  mono?: string;
  custom?: CustomFont[];
}

export interface CustomFont {
  name: string;
  url: string;
  weight: string;
  style: string;
}

export interface ChatConfig {
  enabled: boolean;
  default_model: string | null;
  available_models: string[];
  file_uploads_enabled: boolean;
  max_file_size_bytes: number;
  allowed_file_types: string[];
}

export interface AdminConfig {
  enabled: boolean;
}

export interface AuthConfig {
  methods: AuthMethod[];
  oidc: OidcConfig | null;
}

export type AuthMethod = "none" | "api_key" | "oidc" | "header" | "session" | "per_org_sso";

export interface SovereigntyUiConfig {
  custom_fields: CustomSovereigntyFieldDef[];
}

export interface CustomSovereigntyFieldDef {
  key: string;
  title: string;
  description?: string;
}

export interface OidcConfig {
  provider: string;
  authorization_url: string;
  client_id: string;
}
