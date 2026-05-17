mod access_reviews;
mod api_keys;
pub mod audit_logs;
#[cfg(not(target_arch = "wasm32"))]
pub mod background_executor;
#[cfg(not(target_arch = "wasm32"))]
pub mod container_session;
#[cfg(not(target_arch = "wasm32"))]
pub mod containers;
mod conversations;
#[cfg(any(
    feature = "document-extraction-basic",
    feature = "document-extraction-full"
))]
pub mod document_processor;
#[cfg(feature = "sso")]
mod domain_verifications;
mod file_search;
pub mod file_search_tool;
mod file_storage;
mod files;
#[cfg(feature = "forecasting")]
pub mod forecasting;
#[cfg(not(target_arch = "wasm32"))]
pub mod input_file_staging;
mod model_pricing;
pub mod oauth_pkce;
mod org_rbac_policies;
#[cfg(feature = "sso")]
mod org_sso_configs;
mod organizations;
mod projects;
#[cfg(feature = "prometheus")]
pub mod prometheus_client;
#[cfg(feature = "prometheus")]
pub mod prometheus_parser;
pub mod provider_metrics;
mod providers;
mod reranker;
#[cfg(not(target_arch = "wasm32"))]
pub mod response_event_buffer;
#[cfg(not(target_arch = "wasm32"))]
pub mod response_persister;
#[cfg(not(target_arch = "wasm32"))]
pub mod responses_pipeline;
#[cfg(not(target_arch = "wasm32"))]
mod responses_store;
#[cfg(not(target_arch = "wasm32"))]
pub mod responses_webhook;
#[cfg(feature = "sso")]
mod scim_configs;
#[cfg(feature = "sso")]
mod scim_provisioning;
#[cfg(not(target_arch = "wasm32"))]
pub mod server_tools;
mod service_accounts;
#[cfg(not(target_arch = "wasm32"))]
pub mod shell_tool;
mod skills;
#[cfg(feature = "sso")]
mod sso_group_mappings;
mod teams;
mod templates;
mod usage;
mod users;
mod vector_stores;
#[cfg(feature = "virus-scan")]
mod virus_scan;
pub mod web_search_tool;

use std::sync::Arc;

pub use access_reviews::AccessReviewService;
pub use api_keys::ApiKeyService;
pub use audit_logs::AuditLogService;
pub use conversations::ConversationService;
#[cfg(any(
    feature = "document-extraction-basic",
    feature = "document-extraction-full"
))]
pub use document_processor::{
    DocumentProcessor, DocumentProcessorConfig, DocumentProcessorError, WorkerConfig,
    start_file_processing_worker,
};
#[cfg(feature = "sso")]
pub use domain_verifications::{DomainVerificationError, DomainVerificationService};
pub use file_search::{
    FileSearchError, FileSearchRequest, FileSearchResponse, FileSearchResult, FileSearchService,
    FileSearchServiceConfig,
};
pub use file_search_tool::{
    FileSearchAuthContext, FileSearchContext, FileSearchToolArguments, preprocess_file_search_tools,
};
#[cfg(feature = "server")]
pub use file_storage::FilesystemFileStorage;
#[cfg(feature = "s3-storage")]
pub use file_storage::S3FileStorage;
pub use file_storage::{
    DatabaseFileStorage, FileStorage, FileStorageError, FileStorageResult, create_file_storage,
};
pub use files::{FilesService, FilesServiceError, FilesServiceResult};
pub use model_pricing::ModelPricingService;
pub use oauth_pkce::{OAuthPkceError, OAuthPkceService};
pub use org_rbac_policies::{OrgRbacPolicyError, OrgRbacPolicyService};
#[cfg(feature = "sso")]
pub use org_sso_configs::{OrgSsoConfigError, OrgSsoConfigService, OrgSsoConfigWithClientSecret};
pub use organizations::OrganizationService;
pub use projects::ProjectService;
pub use provider_metrics::{
    ProviderMetricsError, ProviderMetricsService, ProviderStats, ProviderStatsHistorical,
    StatsGranularity, TimeBucketStats,
};
pub use providers::{
    DynamicProviderError, DynamicProviderService, validate_provider_config_with_url,
    validate_provider_type,
};
pub use reranker::{
    LlmReranker, NoOpReranker, RankedResult, RerankError, RerankRequest, RerankResponse,
    RerankUsage, Reranker,
};
#[cfg(not(target_arch = "wasm32"))]
pub use response_event_buffer::ResponseEventBuffer;
#[cfg(not(target_arch = "wasm32"))]
pub use responses_store::{
    CancelSignal, ResponsesStore, ResponsesStoreError, ResponsesStoreResult,
};
#[cfg(not(target_arch = "wasm32"))]
pub use responses_webhook::{ResponsesWebhookDispatcher, WebhookEvent, WebhookEventData};
#[cfg(feature = "sso")]
pub use scim_configs::{OrgScimConfigError, OrgScimConfigService};
#[cfg(feature = "sso")]
pub use scim_provisioning::ScimProvisioningService;
pub use service_accounts::ServiceAccountService;
pub use skills::SkillService;
#[cfg(feature = "sso")]
pub use sso_group_mappings::SsoGroupMappingService;
pub use teams::TeamService;
pub use templates::TemplateService;
pub use usage::UsageService;
pub use users::UserService;
pub use vector_stores::VectorStoresService;
#[cfg(feature = "virus-scan")]
pub use virus_scan::{
    ClamAvScanner, NoOpScanner, ScanResult, VirusScanError, VirusScanResult, VirusScanner,
};
pub use web_search_tool::{WebSearchContext, preprocess_web_search_tools};

use crate::{db::DbPool, events::EventBus};

/// Container for all services
#[derive(Clone)]
pub struct Services {
    pub organizations: OrganizationService,
    pub teams: TeamService,
    pub projects: ProjectService,
    pub users: UserService,
    pub api_keys: ApiKeyService,
    pub providers: DynamicProviderService,
    pub usage: UsageService,
    pub model_pricing: ModelPricingService,
    pub conversations: ConversationService,
    pub templates: TemplateService,
    pub skills: SkillService,
    pub audit_logs: AuditLogService,
    pub access_reviews: AccessReviewService,
    pub vector_stores: VectorStoresService,
    pub files: FilesService,
    #[cfg(feature = "sso")]
    pub sso_group_mappings: SsoGroupMappingService,
    #[cfg(feature = "sso")]
    pub org_sso_configs: OrgSsoConfigService,
    #[cfg(feature = "sso")]
    pub domain_verifications: DomainVerificationService,
    #[cfg(feature = "sso")]
    pub scim_configs: OrgScimConfigService,
    #[cfg(feature = "sso")]
    pub scim_provisioning: ScimProvisioningService,
    pub org_rbac_policies: OrgRbacPolicyService,
    pub service_accounts: ServiceAccountService,
    pub oauth_pkce: OAuthPkceService,
}

impl Services {
    pub fn new(
        db: Arc<DbPool>,
        file_storage: Arc<dyn FileStorage>,
        max_expression_length: usize,
        max_skill_bytes: u32,
    ) -> Self {
        Self {
            organizations: OrganizationService::new(db.clone()),
            teams: TeamService::new(db.clone()),
            projects: ProjectService::new(db.clone()),
            users: UserService::new(db.clone()),
            api_keys: ApiKeyService::new(db.clone()),
            providers: DynamicProviderService::new(db.clone()),
            usage: UsageService::new(db.clone()),
            model_pricing: ModelPricingService::new(db.clone()),
            conversations: ConversationService::new(db.clone()),
            templates: TemplateService::new(db.clone()),
            skills: SkillService::new(db.clone(), max_skill_bytes),
            audit_logs: AuditLogService::new(db.clone()),
            access_reviews: AccessReviewService::new(db.clone()),
            vector_stores: VectorStoresService::new(db.clone()),
            #[cfg(feature = "sso")]
            sso_group_mappings: SsoGroupMappingService::new(db.clone()),
            #[cfg(feature = "sso")]
            org_sso_configs: OrgSsoConfigService::new(db.clone()),
            #[cfg(feature = "sso")]
            domain_verifications: DomainVerificationService::new(db.clone()),
            #[cfg(feature = "sso")]
            scim_configs: OrgScimConfigService::new(db.clone()),
            #[cfg(feature = "sso")]
            scim_provisioning: ScimProvisioningService::new(db.clone()),
            org_rbac_policies: OrgRbacPolicyService::new(db.clone(), max_expression_length),
            service_accounts: ServiceAccountService::new(db.clone()),
            oauth_pkce: OAuthPkceService::new(db.clone()),
            files: FilesService::new(db, file_storage),
        }
    }

    /// Create services with an EventBus for real-time event broadcasting.
    pub fn with_event_bus(
        db: Arc<DbPool>,
        file_storage: Arc<dyn FileStorage>,
        event_bus: Arc<EventBus>,
        max_expression_length: usize,
        max_skill_bytes: u32,
    ) -> Self {
        Self {
            organizations: OrganizationService::new(db.clone()),
            teams: TeamService::new(db.clone()),
            projects: ProjectService::new(db.clone()),
            users: UserService::new(db.clone()),
            api_keys: ApiKeyService::new(db.clone()),
            providers: DynamicProviderService::new(db.clone()),
            usage: UsageService::new(db.clone()),
            model_pricing: ModelPricingService::new(db.clone()),
            conversations: ConversationService::new(db.clone()),
            templates: TemplateService::new(db.clone()),
            skills: SkillService::new(db.clone(), max_skill_bytes),
            audit_logs: AuditLogService::with_event_bus(db.clone(), event_bus),
            access_reviews: AccessReviewService::new(db.clone()),
            vector_stores: VectorStoresService::new(db.clone()),
            #[cfg(feature = "sso")]
            sso_group_mappings: SsoGroupMappingService::new(db.clone()),
            #[cfg(feature = "sso")]
            org_sso_configs: OrgSsoConfigService::new(db.clone()),
            #[cfg(feature = "sso")]
            domain_verifications: DomainVerificationService::new(db.clone()),
            #[cfg(feature = "sso")]
            scim_configs: OrgScimConfigService::new(db.clone()),
            #[cfg(feature = "sso")]
            scim_provisioning: ScimProvisioningService::new(db.clone()),
            org_rbac_policies: OrgRbacPolicyService::new(db.clone(), max_expression_length),
            service_accounts: ServiceAccountService::new(db.clone()),
            oauth_pkce: OAuthPkceService::new(db.clone()),
            files: FilesService::new(db, file_storage),
        }
    }
}
