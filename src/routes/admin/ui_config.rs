use axum::{Json, extract::State};
use serde::Serialize;

use crate::{
    AppState,
    config::{
        AdminConfig, AdminPagesConfig, AuthMode, BrandingConfig, ChatConfig, ColorPalette,
        CustomFont, FavoriteMcpServer, FontsConfig, LoginConfig, McpUiConfig, PageConfig,
        PageStatus, PagesConfig, UiConfig,
    },
};

/// UI configuration response for frontend applications.
#[derive(Debug, Serialize)]
pub struct UiConfigResponse {
    pub branding: BrandingResponse,
    pub chat: ChatResponse,
    pub admin: AdminResponse,
    pub auth: AuthResponse,
    pub sovereignty: SovereigntyUiResponse,
    pub pages: PagesResponse,
    pub mcp: McpUiResponse,
}

#[derive(Debug, Serialize)]
pub struct McpUiResponse {
    pub favorites: Vec<FavoriteMcpServerResponse>,
}

#[derive(Debug, Serialize)]
pub struct FavoriteMcpServerResponse {
    pub name: String,
    pub url: String,
}

impl From<&McpUiConfig> for McpUiResponse {
    fn from(config: &McpUiConfig) -> Self {
        Self {
            favorites: config
                .favorites
                .iter()
                .map(FavoriteMcpServerResponse::from)
                .collect(),
        }
    }
}

impl From<&FavoriteMcpServer> for FavoriteMcpServerResponse {
    fn from(entry: &FavoriteMcpServer) -> Self {
        Self {
            name: entry.name.clone(),
            url: entry.url.clone(),
        }
    }
}

#[derive(Debug, Serialize)]
pub struct BrandingResponse {
    pub title: String,
    pub tagline: Option<String>,
    pub logo_url: Option<String>,
    pub logo_dark_url: Option<String>,
    pub favicon_url: Option<String>,
    pub colors: ColorPaletteResponse,
    pub colors_dark: Option<ColorPaletteResponse>,
    pub fonts: Option<FontsResponse>,
    pub footer_text: Option<String>,
    pub footer_links: Vec<FooterLinkResponse>,
    pub show_version: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    pub login: Option<LoginResponse>,
}

#[derive(Debug, Serialize)]
pub struct LoginResponse {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub subtitle: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub background_image: Option<String>,
    pub show_logo: bool,
}

#[derive(Debug, Serialize, Default)]
pub struct ColorPaletteResponse {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub primary: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub primary_foreground: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub secondary: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub secondary_foreground: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub accent: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub background: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub foreground: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub muted: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub border: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct FooterLinkResponse {
    pub label: String,
    pub url: String,
}

#[derive(Debug, Serialize)]
pub struct FontsResponse {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub heading: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub body: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mono: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub custom: Vec<CustomFontResponse>,
}

#[derive(Debug, Serialize)]
pub struct CustomFontResponse {
    pub name: String,
    pub url: String,
    pub weight: String,
    pub style: String,
}

impl From<&FontsConfig> for FontsResponse {
    fn from(config: &FontsConfig) -> Self {
        Self {
            heading: config.heading.clone(),
            body: config.body.clone(),
            mono: config.mono.clone(),
            custom: config.custom.iter().map(CustomFontResponse::from).collect(),
        }
    }
}

impl From<&CustomFont> for CustomFontResponse {
    fn from(font: &CustomFont) -> Self {
        Self {
            name: font.name.clone(),
            url: font.url.clone(),
            weight: font.weight.clone(),
            style: font.style.clone(),
        }
    }
}

#[derive(Debug, Serialize)]
pub struct ChatResponse {
    pub enabled: bool,
    pub default_model: Option<String>,
    pub available_models: Vec<String>,
    pub file_uploads_enabled: bool,
    pub max_file_size_bytes: usize,
    pub allowed_file_types: Vec<String>,
}

#[derive(Debug, Serialize)]
pub struct AdminResponse {
    pub enabled: bool,
}

#[derive(Debug, Serialize)]
pub struct AuthResponse {
    /// Auth methods available to the SPA. Values: "none" (no auth),
    /// "api_key" (key login form), "header" (IAP reverse proxy),
    /// "session" (IdP mode — cookie sessions, probe /auth/me),
    /// "per_org_sso" (email discovery for per-org OIDC/SAML). The frontend
    /// treats "session", "per_org_sso", and legacy "oidc" as cookie-session
    /// methods (`COOKIE_SESSION_METHODS` in ui/src/auth/types.ts).
    pub methods: Vec<String>,
    pub oidc: Option<OidcResponse>,
}

#[derive(Debug, Serialize)]
pub struct OidcResponse {
    pub provider: String,
    pub authorization_url: String,
    pub client_id: String,
}

#[derive(Debug, Serialize)]
pub struct SovereigntyUiResponse {
    pub custom_fields: Vec<CustomFieldDefResponse>,
}

#[derive(Debug, Serialize)]
pub struct CustomFieldDefResponse {
    pub key: String,
    pub title: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct PageConfigResponse {
    pub status: PageStatus,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub notice_message: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct PagesResponse {
    pub chat: PageConfigResponse,
    pub studio: PageConfigResponse,
    pub projects: PageConfigResponse,
    pub teams: PageConfigResponse,
    pub knowledge_bases: PageConfigResponse,
    pub containers: PageConfigResponse,
    pub api_keys: PageConfigResponse,
    pub providers: PageConfigResponse,
    pub usage: PageConfigResponse,
    pub admin: AdminPagesResponse,
}

#[derive(Debug, Serialize)]
pub struct AdminPagesResponse {
    pub dashboard: PageConfigResponse,
    pub organizations: PageConfigResponse,
    pub projects: PageConfigResponse,
    pub teams: PageConfigResponse,
    pub service_accounts: PageConfigResponse,
    pub users: PageConfigResponse,
    pub sso: PageConfigResponse,
    pub session_info: PageConfigResponse,
    pub api_keys: PageConfigResponse,
    pub providers: PageConfigResponse,
    pub provider_health: PageConfigResponse,
    pub knowledge_bases: PageConfigResponse,
    pub pricing: PageConfigResponse,
    pub usage: PageConfigResponse,
    pub audit_logs: PageConfigResponse,
    pub settings: PageConfigResponse,
}

impl From<&PageConfig> for PageConfigResponse {
    fn from(config: &PageConfig) -> Self {
        Self {
            status: config.status().clone(),
            notice_message: config.notice_message().map(String::from),
        }
    }
}

impl From<&PagesConfig> for PagesResponse {
    fn from(config: &PagesConfig) -> Self {
        Self {
            chat: PageConfigResponse::from(&config.chat),
            studio: PageConfigResponse::from(&config.studio),
            projects: PageConfigResponse::from(&config.projects),
            teams: PageConfigResponse::from(&config.teams),
            knowledge_bases: PageConfigResponse::from(&config.knowledge_bases),
            containers: PageConfigResponse::from(&config.containers),
            api_keys: PageConfigResponse::from(&config.api_keys),
            providers: PageConfigResponse::from(&config.providers),
            usage: PageConfigResponse::from(&config.usage),
            admin: AdminPagesResponse::from(&config.admin),
        }
    }
}

impl From<&AdminPagesConfig> for AdminPagesResponse {
    fn from(config: &AdminPagesConfig) -> Self {
        Self {
            dashboard: PageConfigResponse::from(&config.dashboard),
            organizations: PageConfigResponse::from(&config.organizations),
            projects: PageConfigResponse::from(&config.projects),
            teams: PageConfigResponse::from(&config.teams),
            service_accounts: PageConfigResponse::from(&config.service_accounts),
            users: PageConfigResponse::from(&config.users),
            sso: PageConfigResponse::from(&config.sso),
            session_info: PageConfigResponse::from(&config.session_info),
            api_keys: PageConfigResponse::from(&config.api_keys),
            providers: PageConfigResponse::from(&config.providers),
            provider_health: PageConfigResponse::from(&config.provider_health),
            knowledge_bases: PageConfigResponse::from(&config.knowledge_bases),
            pricing: PageConfigResponse::from(&config.pricing),
            usage: PageConfigResponse::from(&config.usage),
            audit_logs: PageConfigResponse::from(&config.audit_logs),
            settings: PageConfigResponse::from(&config.settings),
        }
    }
}

impl From<&UiConfig> for UiConfigResponse {
    fn from(config: &UiConfig) -> Self {
        Self {
            branding: BrandingResponse::from(&config.branding),
            chat: ChatResponse::from(&config.chat),
            admin: AdminResponse::from(&config.admin),
            auth: AuthResponse::default(),
            sovereignty: SovereigntyUiResponse {
                custom_fields: vec![],
            },
            pages: PagesResponse::from(&config.pages),
            mcp: McpUiResponse::from(&config.mcp),
        }
    }
}

impl From<&BrandingConfig> for BrandingResponse {
    fn from(config: &BrandingConfig) -> Self {
        Self {
            title: config
                .title
                .clone()
                .unwrap_or_else(|| "Hadrian Gateway".to_string()),
            tagline: config.tagline.clone(),
            logo_url: config.logo_url.clone(),
            logo_dark_url: config.logo_dark_url.clone(),
            favicon_url: config.favicon_url.clone(),
            colors: config
                .colors
                .as_ref()
                .map(ColorPaletteResponse::from)
                .unwrap_or_default(),
            colors_dark: config.colors_dark.as_ref().map(ColorPaletteResponse::from),
            fonts: config.fonts.as_ref().map(FontsResponse::from),
            footer_text: config.footer_text.clone(),
            footer_links: config
                .footer_links
                .iter()
                .map(|l| FooterLinkResponse {
                    label: l.label.clone(),
                    url: l.url.clone(),
                })
                .collect(),
            show_version: config.show_version,
            version: if config.show_version {
                Some(env!("CARGO_PKG_VERSION").to_string())
            } else {
                None
            },
            login: config.login.as_ref().map(LoginResponse::from),
        }
    }
}

impl From<&LoginConfig> for LoginResponse {
    fn from(config: &LoginConfig) -> Self {
        Self {
            title: config.title.clone(),
            subtitle: config.subtitle.clone(),
            background_image: config.background_image.clone(),
            show_logo: config.show_logo,
        }
    }
}

impl From<&ColorPalette> for ColorPaletteResponse {
    fn from(config: &ColorPalette) -> Self {
        Self {
            primary: config.primary.clone(),
            primary_foreground: config.primary_foreground.clone(),
            secondary: config.secondary.clone(),
            secondary_foreground: config.secondary_foreground.clone(),
            accent: config.accent.clone(),
            background: config.background.clone(),
            foreground: config.foreground.clone(),
            muted: config.muted.clone(),
            border: config.border.clone(),
        }
    }
}

impl From<&ChatConfig> for ChatResponse {
    fn from(config: &ChatConfig) -> Self {
        Self {
            enabled: config.enabled,
            default_model: config.default_model.clone(),
            available_models: config.available_models.clone(),
            file_uploads_enabled: config.file_uploads.enabled,
            max_file_size_bytes: config.file_uploads.max_size_bytes,
            allowed_file_types: config.file_uploads.allowed_types.clone(),
        }
    }
}

impl From<&AdminConfig> for AdminResponse {
    fn from(config: &AdminConfig) -> Self {
        Self {
            enabled: config.enabled,
        }
    }
}

impl Default for AuthResponse {
    fn default() -> Self {
        Self {
            methods: vec!["api_key".to_string()],
            oidc: None,
        }
    }
}

/// Get UI configuration for frontend applications.
/// This endpoint is unauthenticated so the UI can fetch it before login.
pub async fn get_ui_config(State(state): State<AppState>) -> Json<UiConfigResponse> {
    let ui_config = &state.config.ui;
    let mut response = UiConfigResponse::from(ui_config);

    // With [features.containers] disabled the shell tool never persists
    // containers, so the Containers page would only ever show an empty
    // list — hide it regardless of [ui.pages] settings.
    if !state.config.features.containers.enabled {
        response.pages.containers.status = PageStatus::Disabled;
    }

    // Add sovereignty custom field definitions
    response.sovereignty = SovereigntyUiResponse {
        custom_fields: state
            .config
            .sovereignty
            .custom_fields
            .iter()
            .map(|f| CustomFieldDefResponse {
                key: f.key.clone(),
                title: f.title.clone(),
                description: f.description.clone(),
            })
            .collect(),
    };

    // Add auth methods based on configuration
    let mut auth_methods = Vec::new();

    // Add auth methods based on the configured auth mode
    match &state.config.auth.mode {
        AuthMode::None => {
            // No auth - fall through to "none" below
        }
        AuthMode::ApiKey => {
            // API key mode - offer API key login for admin panel
            auth_methods.push("api_key".to_string());
        }
        #[cfg(feature = "sso")]
        AuthMode::Idp => {
            // IdP mode - users authenticate via per-org SSO
            // The frontend should show email discovery to determine which org's IdP to use
            auth_methods.push("session".to_string());
        }
        AuthMode::Iap(_) => {
            // IAP mode - reverse proxy handles auth
            auth_methods.push("header".to_string());
        }
    }

    // Check if any per-org SSO configurations exist (for SAML or per-org OIDC)
    // This enables email discovery on the login page even when no global OIDC is configured
    #[cfg(feature = "sso")]
    if let Some(ref services) = state.services
        && services
            .org_sso_configs
            .any_enabled()
            .await
            .unwrap_or(false)
    {
        auth_methods.push("per_org_sso".to_string());
    }

    // If no auth is configured at all, allow unauthenticated access
    if auth_methods.is_empty() {
        auth_methods.push("none".to_string());
    }

    response.auth.methods = auth_methods;

    Json(response)
}
