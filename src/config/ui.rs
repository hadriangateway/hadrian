use serde::{Deserialize, Serialize};

/// UI configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct UiConfig {
    /// Enable the UI.
    #[serde(default)]
    pub enabled: bool,

    /// Path to serve the UI from (default: /).
    #[serde(default = "default_ui_path")]
    pub path: String,

    /// Static assets configuration.
    #[serde(default)]
    pub assets: AssetsConfig,

    /// Chat interface configuration.
    #[serde(default)]
    pub chat: ChatConfig,

    /// Admin panel configuration.
    #[serde(default)]
    pub admin: AdminConfig,

    /// Branding customization.
    #[serde(default)]
    pub branding: BrandingConfig,

    /// Per-page visibility configuration.
    #[serde(default)]
    pub pages: PagesConfig,

    /// MCP (Model Context Protocol) UI configuration.
    #[serde(default)]
    pub mcp: McpUiConfig,
}

impl Default for UiConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            path: default_ui_path(),
            assets: AssetsConfig::default(),
            chat: ChatConfig::default(),
            admin: AdminConfig::default(),
            branding: BrandingConfig::default(),
            pages: PagesConfig::default(),
            mcp: McpUiConfig::default(),
        }
    }
}

/// MCP (Model Context Protocol) UI configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct McpUiConfig {
    /// Favorite MCP servers surfaced prominently in the catalog.
    #[serde(default = "default_favorite_mcp_servers")]
    pub favorites: Vec<FavoriteMcpServer>,
}

impl Default for McpUiConfig {
    fn default() -> Self {
        Self {
            favorites: default_favorite_mcp_servers(),
        }
    }
}

/// A suggested MCP server shown in the catalog's "Favorites" section.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct FavoriteMcpServer {
    /// Display name.
    pub name: String,
    /// Either a direct remote URL (`https://…`) the UI connects to, or a
    /// registry identifier (e.g. `io.github.hadriangateway/platter`) the UI
    /// resolves against the public MCP registry.
    pub url: String,
}

fn default_favorite_mcp_servers() -> Vec<FavoriteMcpServer> {
    vec![
        FavoriteMcpServer {
            name: "Platter".into(),
            url: "io.github.hadriangateway/platter".into(),
        },
        FavoriteMcpServer {
            name: "Atlassian".into(),
            url: "https://mcp.atlassian.com/v1/mcp".into(),
        },
        FavoriteMcpServer {
            name: "Notion".into(),
            url: "https://mcp.notion.com/mcp".into(),
        },
        FavoriteMcpServer {
            name: "Hugging Face".into(),
            url: "https://huggingface.co/mcp".into(),
        },
        FavoriteMcpServer {
            name: "Miro".into(),
            url: "https://mcp.miro.com/".into(),
        },
        FavoriteMcpServer {
            name: "Vercel".into(),
            url: "https://mcp.vercel.com".into(),
        },
    ]
}

fn default_true() -> bool {
    true
}

fn default_ui_path() -> String {
    "/".to_string()
}

/// Static assets configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct AssetsConfig {
    /// Source of static assets.
    #[serde(default)]
    pub source: AssetSource,

    /// Cache control header for static assets.
    #[serde(default = "default_cache_control")]
    pub cache_control: String,
}

impl Default for AssetsConfig {
    fn default() -> Self {
        Self {
            source: AssetSource::default(),
            cache_control: default_cache_control(),
        }
    }
}

fn default_cache_control() -> String {
    "public, max-age=31536000, immutable".to_string()
}

/// Source for static assets.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
#[serde(tag = "type", rename_all = "snake_case")]
#[serde(deny_unknown_fields)]
pub enum AssetSource {
    /// Assets embedded in the binary.
    #[default]
    Embedded,

    /// Assets served from the filesystem.
    Filesystem { path: String },

    /// Assets served from a CDN (UI makes requests directly).
    Cdn { base_url: String },
}

/// Chat interface configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct ChatConfig {
    /// Enable chat interface.
    #[serde(default = "default_true")]
    pub enabled: bool,

    /// Default model for new chats.
    #[serde(default)]
    pub default_model: Option<String>,

    /// Available models in the UI (if empty, all models are shown).
    #[serde(default)]
    pub available_models: Vec<String>,

    /// Enable file uploads.
    #[serde(default)]
    pub file_uploads: FileUploadConfig,
}

impl Default for ChatConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            default_model: None,
            available_models: vec![],
            file_uploads: FileUploadConfig::default(),
        }
    }
}

/// File upload configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct FileUploadConfig {
    /// Enable file uploads.
    #[serde(default)]
    pub enabled: bool,

    /// Maximum file size in bytes.
    #[serde(default = "default_max_file_size")]
    pub max_size_bytes: usize,

    /// Allowed MIME types.
    #[serde(default = "default_allowed_types")]
    pub allowed_types: Vec<String>,

    /// Storage backend for uploaded files.
    #[serde(default)]
    pub storage: UploadStorageConfig,
}

impl Default for FileUploadConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            max_size_bytes: default_max_file_size(),
            allowed_types: default_allowed_types(),
            storage: UploadStorageConfig::default(),
        }
    }
}

fn default_max_file_size() -> usize {
    10 * 1024 * 1024 // 10 MB
}

fn default_allowed_types() -> Vec<String> {
    vec![
        "image/png".into(),
        "image/jpeg".into(),
        "image/gif".into(),
        "image/webp".into(),
        "application/pdf".into(),
        "text/plain".into(),
        "text/markdown".into(),
    ]
}

/// Storage backend for chat file uploads.
///
/// Note: For the Files API storage backend, see `FileStorageConfig` in `storage.rs`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
#[serde(tag = "type", rename_all = "snake_case")]
#[serde(deny_unknown_fields)]
pub enum UploadStorageConfig {
    /// Store in database (for small files).
    #[default]
    Database,

    /// Store on local filesystem.
    Filesystem { path: String },

    /// Store in S3-compatible storage.
    S3 {
        bucket: String,
        #[serde(default)]
        region: Option<String>,
        #[serde(default)]
        endpoint: Option<String>,
        #[serde(default)]
        prefix: Option<String>,
    },
}

/// Admin panel configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct AdminConfig {
    /// Enable admin panel.
    #[serde(default = "default_true")]
    pub enabled: bool,

    /// Path for admin panel.
    #[serde(default = "default_admin_path")]
    pub path: String,
}

impl Default for AdminConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            path: default_admin_path(),
        }
    }
}

fn default_admin_path() -> String {
    "/admin".to_string()
}

/// Branding customization.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct BrandingConfig {
    /// Application title.
    #[serde(default)]
    pub title: Option<String>,

    /// Tagline shown below the title (e.g., "Powering research with AI").
    #[serde(default)]
    pub tagline: Option<String>,

    /// Logo URL.
    #[serde(default)]
    pub logo_url: Option<String>,

    /// Logo URL for dark mode. Falls back to logo_url if not specified.
    #[serde(default)]
    pub logo_dark_url: Option<String>,

    /// Favicon URL.
    #[serde(default)]
    pub favicon_url: Option<String>,

    /// Color palette for light mode.
    #[serde(default)]
    pub colors: Option<ColorPalette>,

    /// Color palette overrides for dark mode.
    #[serde(default)]
    pub colors_dark: Option<ColorPalette>,

    /// Typography configuration.
    #[serde(default)]
    pub fonts: Option<FontsConfig>,

    /// Custom CSS URL.
    #[serde(default)]
    pub custom_css_url: Option<String>,

    /// Footer text.
    #[serde(default)]
    pub footer_text: Option<String>,

    /// Footer links.
    #[serde(default)]
    pub footer_links: Vec<FooterLink>,

    /// Show version in footer.
    #[serde(default)]
    pub show_version: bool,

    /// Login page customization.
    #[serde(default)]
    pub login: Option<LoginConfig>,
}

/// Color palette for branding customization.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct ColorPalette {
    /// Primary brand color (hex, e.g., "#3b82f6").
    #[serde(default)]
    pub primary: Option<String>,

    /// Text color on primary backgrounds (hex, e.g., "#ffffff").
    /// Used for text on primary buttons like "New Chat".
    #[serde(default)]
    pub primary_foreground: Option<String>,

    /// Secondary color for secondary actions (hex).
    #[serde(default)]
    pub secondary: Option<String>,

    /// Text color on secondary backgrounds (hex).
    #[serde(default)]
    pub secondary_foreground: Option<String>,

    /// Accent color for highlights and CTAs (hex).
    #[serde(default)]
    pub accent: Option<String>,

    /// Background color (hex).
    #[serde(default)]
    pub background: Option<String>,

    /// Foreground/text color (hex).
    #[serde(default)]
    pub foreground: Option<String>,

    /// Muted color for subtle backgrounds (hex).
    #[serde(default)]
    pub muted: Option<String>,

    /// Border color (hex).
    #[serde(default)]
    pub border: Option<String>,
}

/// Typography/font configuration.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct FontsConfig {
    /// Font family for headings (e.g., "Inter", "Roboto").
    #[serde(default)]
    pub heading: Option<String>,

    /// Font family for body text (e.g., "Inter", "Roboto").
    #[serde(default)]
    pub body: Option<String>,

    /// Font family for monospace/code text (e.g., "JetBrains Mono", "Fira Code").
    #[serde(default)]
    pub mono: Option<String>,

    /// Custom fonts to load via @font-face.
    #[serde(default)]
    pub custom: Vec<CustomFont>,
}

/// Custom font definition for loading external fonts.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct CustomFont {
    /// Font family name to use in CSS.
    pub name: String,

    /// URL to the font file (woff2, woff, ttf, otf).
    pub url: String,

    /// Font weight (e.g., "400", "700", "100 900" for variable fonts).
    #[serde(default = "default_font_weight")]
    pub weight: String,

    /// Font style ("normal" or "italic").
    #[serde(default = "default_font_style")]
    pub style: String,
}

fn default_font_weight() -> String {
    "400".to_string()
}

fn default_font_style() -> String {
    "normal".to_string()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct FooterLink {
    pub label: String,
    pub url: String,
}

/// Page visibility status.
#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum PageStatus {
    #[default]
    Enabled,
    Disabled,
    Notice,
}

/// Per-page configuration. Accepts either a bare string (`"enabled"`) or an inline table
/// (`{ status = "notice", notice_message = "..." }`).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
#[serde(untagged)]
pub enum PageConfig {
    Simple(PageStatus),
    Detailed {
        status: PageStatus,
        #[serde(default)]
        notice_message: Option<String>,
    },
}

impl Default for PageConfig {
    fn default() -> Self {
        Self::Simple(PageStatus::Enabled)
    }
}

impl PageConfig {
    pub fn status(&self) -> &PageStatus {
        match self {
            Self::Simple(s) => s,
            Self::Detailed { status, .. } => status,
        }
    }

    pub fn notice_message(&self) -> Option<&str> {
        match self {
            Self::Simple(_) => None,
            Self::Detailed { notice_message, .. } => notice_message.as_deref(),
        }
    }
}

/// Per-page visibility for main UI pages.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct PagesConfig {
    #[serde(default)]
    pub chat: PageConfig,
    #[serde(default)]
    pub studio: PageConfig,
    #[serde(default)]
    pub projects: PageConfig,
    #[serde(default)]
    pub teams: PageConfig,
    #[serde(default)]
    pub knowledge_bases: PageConfig,
    #[serde(default)]
    pub containers: PageConfig,
    #[serde(default)]
    pub api_keys: PageConfig,
    #[serde(default)]
    pub providers: PageConfig,
    #[serde(default)]
    pub usage: PageConfig,
    #[serde(default)]
    pub admin: AdminPagesConfig,
}

/// Per-page visibility for admin pages.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct AdminPagesConfig {
    #[serde(default)]
    pub dashboard: PageConfig,
    #[serde(default)]
    pub organizations: PageConfig,
    #[serde(default)]
    pub projects: PageConfig,
    #[serde(default)]
    pub teams: PageConfig,
    #[serde(default)]
    pub service_accounts: PageConfig,
    #[serde(default)]
    pub users: PageConfig,
    #[serde(default)]
    pub sso: PageConfig,
    #[serde(default)]
    pub session_info: PageConfig,
    #[serde(default)]
    pub api_keys: PageConfig,
    #[serde(default)]
    pub providers: PageConfig,
    #[serde(default)]
    pub provider_health: PageConfig,
    #[serde(default)]
    pub knowledge_bases: PageConfig,
    #[serde(default)]
    pub pricing: PageConfig,
    #[serde(default)]
    pub usage: PageConfig,
    #[serde(default)]
    pub audit_logs: PageConfig,
    #[serde(default)]
    pub settings: PageConfig,
}

/// Login page customization.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct LoginConfig {
    /// Custom title for the login page (e.g., "Sign in to AI Gateway").
    #[serde(default)]
    pub title: Option<String>,

    /// Subtitle shown below the title (e.g., "Use your university credentials").
    #[serde(default)]
    pub subtitle: Option<String>,

    /// Background image URL for the login page.
    #[serde(default)]
    pub background_image: Option<String>,

    /// Whether to show the logo on the login page (defaults to true).
    #[serde(default = "default_true")]
    pub show_logo: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn simple_string_status() {
        let toml = r#"chat = "enabled""#;
        let pages: PagesConfig = toml::from_str(toml).unwrap();
        assert_eq!(pages.chat.status(), &PageStatus::Enabled);
    }

    #[test]
    fn detailed_table() {
        let toml = r#"
[chat]
status = "notice"
notice_message = "Under maintenance"
"#;
        let pages: PagesConfig = toml::from_str(toml).unwrap();
        assert_eq!(pages.chat.status(), &PageStatus::Notice);
        assert_eq!(pages.chat.notice_message(), Some("Under maintenance"));
    }

    #[test]
    fn mixed_formats_with_defaults() {
        let toml = r#"
chat = "disabled"
studio = "enabled"
"#;
        let pages: PagesConfig = toml::from_str(toml).unwrap();
        assert_eq!(pages.chat.status(), &PageStatus::Disabled);
        assert_eq!(pages.studio.status(), &PageStatus::Enabled);
        // Omitted fields default to enabled
        assert_eq!(pages.teams.status(), &PageStatus::Enabled);
        assert_eq!(pages.usage.status(), &PageStatus::Enabled);
    }

    #[test]
    fn invalid_status_fails() {
        let toml = r#"chat = "bogus""#;
        assert!(toml::from_str::<PagesConfig>(toml).is_err());
    }

    #[test]
    fn unknown_field_rejected() {
        let toml = r#"nonexistent_page = "enabled""#;
        assert!(toml::from_str::<PagesConfig>(toml).is_err());
    }
}
