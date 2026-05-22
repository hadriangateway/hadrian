-- Initial schema for Hadrian Gateway (SQLite)

-- ======================================================================
-- Organizations
-- ======================================================================

CREATE TABLE IF NOT EXISTS organizations (
    id TEXT PRIMARY KEY NOT NULL,
    slug TEXT NOT NULL UNIQUE,
    name TEXT NOT NULL,
    created_at TEXT NOT NULL DEFAULT (datetime('now')),
    updated_at TEXT NOT NULL DEFAULT (datetime('now')),
    deleted_at TEXT
);

CREATE INDEX IF NOT EXISTS idx_organizations_slug ON organizations(slug);
-- Partial index for non-deleted organizations (most queries filter by deleted_at IS NULL)
CREATE INDEX IF NOT EXISTS idx_organizations_slug_active ON organizations(slug) WHERE deleted_at IS NULL;

-- ======================================================================
-- Teams
-- ======================================================================

-- Groups within organizations
CREATE TABLE IF NOT EXISTS teams (
    id TEXT PRIMARY KEY NOT NULL,
    org_id TEXT NOT NULL REFERENCES organizations(id) ON DELETE CASCADE,
    slug TEXT NOT NULL,
    name TEXT NOT NULL,
    created_at TEXT NOT NULL DEFAULT (datetime('now')),
    updated_at TEXT NOT NULL DEFAULT (datetime('now')),
    deleted_at TEXT,
    UNIQUE(org_id, slug)
);

CREATE INDEX IF NOT EXISTS idx_teams_org_id ON teams(org_id);
CREATE INDEX IF NOT EXISTS idx_teams_slug ON teams(slug);
-- Partial indexes for non-deleted teams (most queries filter by deleted_at IS NULL)
CREATE INDEX IF NOT EXISTS idx_teams_org_active ON teams(org_id) WHERE deleted_at IS NULL;
CREATE INDEX IF NOT EXISTS idx_teams_org_slug_active ON teams(org_id, slug) WHERE deleted_at IS NULL;

-- ======================================================================
-- Projects
-- ======================================================================

-- Belong to organizations, optionally to teams
CREATE TABLE IF NOT EXISTS projects (
    id TEXT PRIMARY KEY NOT NULL,
    org_id TEXT NOT NULL REFERENCES organizations(id) ON DELETE CASCADE,
    team_id TEXT REFERENCES teams(id) ON DELETE SET NULL,
    slug TEXT NOT NULL,
    name TEXT NOT NULL,
    created_at TEXT NOT NULL DEFAULT (datetime('now')),
    updated_at TEXT NOT NULL DEFAULT (datetime('now')),
    deleted_at TEXT,
    UNIQUE(org_id, slug)
);

CREATE INDEX IF NOT EXISTS idx_projects_org_id ON projects(org_id);
CREATE INDEX IF NOT EXISTS idx_projects_slug ON projects(slug);
CREATE INDEX IF NOT EXISTS idx_projects_team_id ON projects(team_id) WHERE team_id IS NOT NULL;
-- Partial indexes for non-deleted projects (most queries filter by deleted_at IS NULL)
CREATE INDEX IF NOT EXISTS idx_projects_org_active ON projects(org_id) WHERE deleted_at IS NULL;
CREATE INDEX IF NOT EXISTS idx_projects_org_slug_active ON projects(org_id, slug) WHERE deleted_at IS NULL;

-- ======================================================================
-- Users
-- ======================================================================

-- External identity, linked via external_id
CREATE TABLE IF NOT EXISTS users (
    id TEXT PRIMARY KEY NOT NULL,
    external_id TEXT NOT NULL UNIQUE,
    email TEXT UNIQUE,
    name TEXT,
    created_at TEXT NOT NULL DEFAULT (datetime('now')),
    updated_at TEXT NOT NULL DEFAULT (datetime('now'))
);

CREATE INDEX IF NOT EXISTS idx_users_external_id ON users(external_id);
CREATE INDEX IF NOT EXISTS idx_users_email ON users(email);

-- ======================================================================
-- Organization Memberships
-- ======================================================================

-- Users belong to organizations
-- source: 'manual' (admin/API), 'jit' (SSO login), 'scim' (IdP push)
CREATE TABLE IF NOT EXISTS org_memberships (
    org_id TEXT NOT NULL REFERENCES organizations(id) ON DELETE CASCADE,
    user_id TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    role TEXT NOT NULL DEFAULT 'member',
    source TEXT NOT NULL DEFAULT 'manual' CHECK (source IN ('manual', 'jit', 'scim')),
    created_at TEXT NOT NULL DEFAULT (datetime('now')),
    PRIMARY KEY (org_id, user_id)
);

CREATE INDEX IF NOT EXISTS idx_org_members_user_id ON org_memberships(user_id);
-- Index for querying memberships by source (used by sync_memberships_on_login)
CREATE INDEX IF NOT EXISTS idx_org_members_source ON org_memberships(user_id, source);
-- Unique index enforcing single-org membership: each user can belong to at most one organization.
-- This prevents race conditions in add_to_org and provides database-level enforcement.
CREATE UNIQUE INDEX IF NOT EXISTS idx_org_memberships_single_org ON org_memberships(user_id);

-- ======================================================================
-- Project Memberships
-- ======================================================================

-- Users belong to projects
-- source: 'manual' (admin/API), 'jit' (SSO login), 'scim' (IdP push)
CREATE TABLE IF NOT EXISTS project_memberships (
    project_id TEXT NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
    user_id TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    role TEXT NOT NULL DEFAULT 'member',
    source TEXT NOT NULL DEFAULT 'manual' CHECK (source IN ('manual', 'jit', 'scim')),
    created_at TEXT NOT NULL DEFAULT (datetime('now')),
    PRIMARY KEY (project_id, user_id)
);

CREATE INDEX IF NOT EXISTS idx_project_members_user_id ON project_memberships(user_id);
-- Index for querying memberships by source
CREATE INDEX IF NOT EXISTS idx_project_members_source ON project_memberships(user_id, source);

-- ======================================================================
-- Team Memberships
-- ======================================================================

-- Users belong to teams
-- source: 'manual' (admin/API), 'jit' (SSO login), 'scim' (IdP push)
CREATE TABLE IF NOT EXISTS team_memberships (
    team_id TEXT NOT NULL REFERENCES teams(id) ON DELETE CASCADE,
    user_id TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    role TEXT NOT NULL DEFAULT 'member',
    source TEXT NOT NULL DEFAULT 'manual' CHECK (source IN ('manual', 'jit', 'scim')),
    created_at TEXT NOT NULL DEFAULT (datetime('now')),
    PRIMARY KEY (team_id, user_id)
);

CREATE INDEX IF NOT EXISTS idx_team_members_user_id ON team_memberships(user_id);
CREATE INDEX IF NOT EXISTS idx_team_members_team_id ON team_memberships(team_id);
-- Index for querying memberships by source (used by sync_memberships_on_login)
CREATE INDEX IF NOT EXISTS idx_team_members_source ON team_memberships(user_id, source);

-- ======================================================================
-- SSO Group Mappings
-- ======================================================================

-- Maps IdP groups to Hadrian teams/roles for JIT provisioning.
-- When a user logs in via SSO, their IdP groups are looked up in this table
-- to determine which teams they should be added to.
-- Multiple mappings per IdP group are allowed (e.g., one group -> multiple teams).
CREATE TABLE IF NOT EXISTS sso_group_mappings (
    id TEXT PRIMARY KEY NOT NULL,
    -- Organization context (required - mappings are org-scoped)
    org_id TEXT NOT NULL REFERENCES organizations(id) ON DELETE CASCADE,
    -- Optional: Team to add user to when they have this IdP group
    team_id TEXT REFERENCES teams(id) ON DELETE CASCADE,
    -- Which SSO connection this mapping applies to (from config)
    sso_connection_name TEXT NOT NULL DEFAULT 'default',
    -- The IdP group name (exactly as it appears in the groups claim)
    idp_group TEXT NOT NULL,
    -- Optional: Role to assign (within the team if team_id set, otherwise org role)
    role TEXT,
    -- Priority for role precedence (higher = wins when multiple mappings target same team)
    priority INTEGER NOT NULL DEFAULT 0,
    created_at TEXT NOT NULL DEFAULT (datetime('now')),
    updated_at TEXT NOT NULL DEFAULT (datetime('now'))
);

-- Index for looking up mappings by SSO connection and org
CREATE INDEX IF NOT EXISTS idx_sso_group_mappings_connection_org ON sso_group_mappings(sso_connection_name, org_id);
-- Index for looking up mappings by IdP group (for resolving user's groups)
CREATE INDEX IF NOT EXISTS idx_sso_group_mappings_idp_group ON sso_group_mappings(idp_group);
-- Index for org-scoped queries
CREATE INDEX IF NOT EXISTS idx_sso_group_mappings_org_id ON sso_group_mappings(org_id);
-- Unique constraint: prevent duplicate mappings (same connection + group + org + team combination)
-- team_id can be NULL, so we need partial indexes for uniqueness
CREATE UNIQUE INDEX IF NOT EXISTS idx_sso_group_mappings_unique_with_team
    ON sso_group_mappings(sso_connection_name, idp_group, org_id, team_id) WHERE team_id IS NOT NULL;
CREATE UNIQUE INDEX IF NOT EXISTS idx_sso_group_mappings_unique_without_team
    ON sso_group_mappings(sso_connection_name, idp_group, org_id) WHERE team_id IS NULL;

-- ======================================================================
-- Organization SSO Configurations
-- ======================================================================

-- Per-org OIDC/SAML settings for multi-tenant SSO.
-- Each organization can have its own IdP configuration.
-- When a user logs in, the system routes to the correct IdP based on email domain.
CREATE TABLE IF NOT EXISTS org_sso_configs (
    id TEXT PRIMARY KEY NOT NULL,
    -- Organization this SSO config belongs to (one SSO config per org)
    org_id TEXT NOT NULL UNIQUE REFERENCES organizations(id) ON DELETE CASCADE,
    -- Provider type: 'oidc' or 'saml'
    provider_type TEXT NOT NULL DEFAULT 'oidc' CHECK (provider_type IN ('oidc', 'saml')),

    -- ==========================================================================
    -- OIDC Configuration (used when provider_type = 'oidc')
    -- ==========================================================================
    -- OIDC issuer URL (e.g., "https://accounts.google.com")
    -- Required for OIDC, NULL for SAML
    issuer TEXT,
    -- OIDC discovery URL (defaults to issuer/.well-known/openid-configuration)
    discovery_url TEXT,
    -- OAuth2 client ID (required for OIDC, NULL for SAML)
    client_id TEXT,
    -- Client secret stored in secret manager, this is the key reference
    -- Required for OIDC, NULL for SAML
    client_secret_key TEXT,
    -- Redirect URI (optional - can use global default)
    redirect_uri TEXT,
    -- Scopes as space-separated string (e.g., 'openid email profile groups')
    scopes TEXT NOT NULL DEFAULT 'openid email profile',
    -- Claims configuration (OIDC-specific)
    identity_claim TEXT,
    org_claim TEXT,
    groups_claim TEXT,

    -- ==========================================================================
    -- SAML 2.0 Configuration (used when provider_type = 'saml')
    -- ==========================================================================
    -- IdP metadata URL for auto-configuration (alternative to manual config)
    saml_metadata_url TEXT,
    -- IdP entity identifier (e.g., "https://idp.example.com/metadata")
    saml_idp_entity_id TEXT,
    -- IdP Single Sign-On service URL (HTTP-Redirect or HTTP-POST binding)
    saml_idp_sso_url TEXT,
    -- IdP Single Logout service URL (optional)
    saml_idp_slo_url TEXT,
    -- IdP X.509 certificate for signature validation (PEM format)
    saml_idp_certificate TEXT,
    -- Service Provider entity ID (Hadrian's identifier to the IdP)
    saml_sp_entity_id TEXT,
    -- NameID format to request (e.g., 'urn:oasis:names:tc:SAML:1.1:nameid-format:emailAddress')
    saml_name_id_format TEXT,
    -- Whether to sign AuthnRequests
    saml_sign_requests INTEGER NOT NULL DEFAULT 0,
    -- SP private key reference in secret manager (used for signing requests)
    saml_sp_private_key_ref TEXT,
    -- SP X.509 certificate for metadata (PEM format, not a secret)
    saml_sp_certificate TEXT,
    -- Whether to force re-authentication at IdP
    saml_force_authn INTEGER NOT NULL DEFAULT 0,
    -- Requested authentication context class
    saml_authn_context_class_ref TEXT,
    -- SAML attribute name for user identity (like identity_claim for OIDC)
    saml_identity_attribute TEXT,
    -- SAML attribute name for email
    saml_email_attribute TEXT,
    -- SAML attribute name for display name
    saml_name_attribute TEXT,
    -- SAML attribute name for groups
    saml_groups_attribute TEXT,

    -- ==========================================================================
    -- JIT Provisioning (shared by OIDC and SAML)
    -- ==========================================================================
    provisioning_enabled INTEGER NOT NULL DEFAULT 1,
    create_users INTEGER NOT NULL DEFAULT 1,
    default_team_id TEXT REFERENCES teams(id) ON DELETE SET NULL,
    default_org_role TEXT NOT NULL DEFAULT 'member',
    default_team_role TEXT NOT NULL DEFAULT 'member',
    -- JSON array of allowed email domains (e.g., '["acme.com", "acme.io"]')
    allowed_email_domains TEXT,
    sync_attributes_on_login INTEGER NOT NULL DEFAULT 0,
    sync_memberships_on_login INTEGER NOT NULL DEFAULT 1,

    -- ==========================================================================
    -- Status & Enforcement
    -- ==========================================================================
    -- SSO enforcement mode: 'optional' (allow other auth), 'required' (SSO only), 'test' (shadow mode)
    enforcement_mode TEXT NOT NULL DEFAULT 'optional' CHECK (enforcement_mode IN ('optional', 'required', 'test')),
    -- Whether this SSO config is active
    enabled INTEGER NOT NULL DEFAULT 1,
    created_at TEXT NOT NULL DEFAULT (datetime('now')),
    updated_at TEXT NOT NULL DEFAULT (datetime('now'))
);

-- Index for looking up SSO config by org_id (also covered by UNIQUE constraint)
CREATE INDEX IF NOT EXISTS idx_org_sso_configs_org_id ON org_sso_configs(org_id);
-- Index for enabled SSO configs (for IdP discovery)
CREATE INDEX IF NOT EXISTS idx_org_sso_configs_enabled ON org_sso_configs(enabled) WHERE enabled = 1;
-- Index for gateway JWT issuer-based lookup (per-org JWT validation hot path)
CREATE INDEX IF NOT EXISTS idx_org_sso_configs_issuer_enabled
  ON org_sso_configs(issuer, provider_type, enabled) WHERE enabled = 1 AND provider_type = 'oidc';

-- ======================================================================
-- Domain Verifications
-- ======================================================================

-- Verify ownership of email domains for SSO.
-- status: 'pending', 'verified', 'failed'
CREATE TABLE IF NOT EXISTS domain_verifications (
    id TEXT PRIMARY KEY NOT NULL,
    -- SSO config this verification belongs to
    org_sso_config_id TEXT NOT NULL REFERENCES org_sso_configs(id) ON DELETE CASCADE,
    -- The domain being verified (e.g., "acme.com")
    domain TEXT NOT NULL,
    -- Random token for DNS TXT record verification
    verification_token TEXT NOT NULL,
    -- Verification status
    status TEXT NOT NULL DEFAULT 'pending' CHECK (status IN ('pending', 'verified', 'failed')),
    -- The actual DNS TXT record found during verification (for audit)
    dns_txt_record TEXT,
    -- Number of verification attempts
    verification_attempts INTEGER NOT NULL DEFAULT 0,
    -- Last verification attempt timestamp
    last_attempt_at TEXT,
    -- When the domain was successfully verified
    verified_at TEXT,
    -- Optional: require re-verification after this date
    expires_at TEXT,
    created_at TEXT NOT NULL DEFAULT (datetime('now')),
    updated_at TEXT NOT NULL DEFAULT (datetime('now')),
    -- Each domain can only be verified once per SSO config
    UNIQUE(org_sso_config_id, domain)
);

-- Index for looking up verifications by SSO config
CREATE INDEX IF NOT EXISTS idx_domain_verifications_config_id ON domain_verifications(org_sso_config_id);
-- Index for looking up verifications by domain (for discovery)
CREATE INDEX IF NOT EXISTS idx_domain_verifications_domain ON domain_verifications(domain);
-- Index for verified domains (for SSO discovery)
CREATE INDEX IF NOT EXISTS idx_domain_verifications_verified ON domain_verifications(domain, status) WHERE status = 'verified';
-- Index for config+status queries (list_verified_by_config, has_verified_domain)
CREATE INDEX IF NOT EXISTS idx_domain_verifications_config_status ON domain_verifications(org_sso_config_id, status);

-- ======================================================================
-- SCIM 2.0 Provisioning
-- ======================================================================

-- Per-organization SCIM configuration.
-- Enables automatic user provisioning/deprovisioning from IdPs (Okta, Azure AD, etc.)
CREATE TABLE IF NOT EXISTS org_scim_configs (
    id TEXT PRIMARY KEY NOT NULL,
    -- Organization this SCIM config belongs to (one SCIM config per org)
    org_id TEXT NOT NULL UNIQUE REFERENCES organizations(id) ON DELETE CASCADE,
    -- Whether SCIM provisioning is enabled
    enabled INTEGER NOT NULL DEFAULT 1,
    -- Bearer token hash for SCIM API authentication
    token_hash TEXT NOT NULL,
    -- Token prefix for identification (first 8 chars, like 'scim_xxxx')
    token_prefix TEXT NOT NULL,
    -- Last time the SCIM token was used
    token_last_used_at TEXT,
    -- Provisioning settings
    create_users INTEGER NOT NULL DEFAULT 1,
    default_team_id TEXT REFERENCES teams(id) ON DELETE SET NULL,
    default_org_role TEXT NOT NULL DEFAULT 'member',
    default_team_role TEXT NOT NULL DEFAULT 'member',
    -- Whether to sync display name from SCIM
    sync_display_name INTEGER NOT NULL DEFAULT 1,
    -- Deprovisioning behavior: delete user entirely (false = just deactivate)
    deactivate_deletes_user INTEGER NOT NULL DEFAULT 0,
    -- Whether to revoke all API keys when user is deactivated via SCIM
    revoke_api_keys_on_deactivate INTEGER NOT NULL DEFAULT 1,
    created_at TEXT NOT NULL DEFAULT (datetime('now')),
    updated_at TEXT NOT NULL DEFAULT (datetime('now'))
);

CREATE INDEX IF NOT EXISTS idx_org_scim_configs_org_id ON org_scim_configs(org_id);
CREATE INDEX IF NOT EXISTS idx_org_scim_configs_enabled ON org_scim_configs(enabled) WHERE enabled = 1;
-- Index for token authentication lookups
CREATE INDEX IF NOT EXISTS idx_org_scim_configs_token_prefix ON org_scim_configs(token_prefix);

-- ======================================================================
-- SCIM User Mappings
-- ======================================================================

-- Maps SCIM external IDs to Hadrian user IDs (per-org).
-- Allows the same user to have different SCIM IDs in different orgs
-- and tracks the SCIM-specific "active" state separately from user deletion.
CREATE TABLE IF NOT EXISTS scim_user_mappings (
    id TEXT PRIMARY KEY NOT NULL,
    org_id TEXT NOT NULL REFERENCES organizations(id) ON DELETE CASCADE,
    -- Hadrian user this maps to
    user_id TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    -- SCIM external ID from IdP (e.g., Okta user ID like '00u1a2b3c4d5e6f7g8h9')
    scim_external_id TEXT NOT NULL,
    -- SCIM "active" status (separate from user existence)
    active INTEGER NOT NULL DEFAULT 1,
    created_at TEXT NOT NULL DEFAULT (datetime('now')),
    updated_at TEXT NOT NULL DEFAULT (datetime('now')),
    -- Each SCIM external ID can only map to one user per org
    UNIQUE(org_id, scim_external_id)
);

CREATE INDEX IF NOT EXISTS idx_scim_user_mappings_org_id ON scim_user_mappings(org_id);
CREATE INDEX IF NOT EXISTS idx_scim_user_mappings_user_id ON scim_user_mappings(user_id);
CREATE INDEX IF NOT EXISTS idx_scim_user_mappings_scim_external_id ON scim_user_mappings(org_id, scim_external_id);

-- ======================================================================
-- SCIM Group Mappings
-- ======================================================================

-- Maps SCIM groups to Hadrian teams (per-org).
-- When a SCIM group is pushed from the IdP, it maps to a Hadrian team.
CREATE TABLE IF NOT EXISTS scim_group_mappings (
    id TEXT PRIMARY KEY NOT NULL,
    org_id TEXT NOT NULL REFERENCES organizations(id) ON DELETE CASCADE,
    -- Hadrian team this maps to
    team_id TEXT NOT NULL REFERENCES teams(id) ON DELETE CASCADE,
    -- SCIM group ID from IdP
    scim_group_id TEXT NOT NULL,
    -- Display name from SCIM (for reference)
    display_name TEXT,
    created_at TEXT NOT NULL DEFAULT (datetime('now')),
    updated_at TEXT NOT NULL DEFAULT (datetime('now')),
    -- Each SCIM group can only map to one team per org
    UNIQUE(org_id, scim_group_id)
);

CREATE INDEX IF NOT EXISTS idx_scim_group_mappings_org_id ON scim_group_mappings(org_id);
CREATE INDEX IF NOT EXISTS idx_scim_group_mappings_team_id ON scim_group_mappings(team_id);
CREATE INDEX IF NOT EXISTS idx_scim_group_mappings_scim_group_id ON scim_group_mappings(org_id, scim_group_id);

-- ======================================================================
-- Organization RBAC Policies
-- ======================================================================

-- Per-organization CEL-based authorization policies for runtime policy management.
-- effect: 'allow' or 'deny' (explicit allow/deny semantic)
-- priority: Higher priority policies are evaluated first (descending order)
CREATE TABLE IF NOT EXISTS org_rbac_policies (
    id TEXT PRIMARY KEY NOT NULL,
    org_id TEXT NOT NULL REFERENCES organizations(id) ON DELETE CASCADE,
    name TEXT NOT NULL,
    description TEXT,
    -- Resource pattern (e.g., 'projects/*', 'teams/engineering/*', '*')
    resource TEXT NOT NULL DEFAULT '*',
    -- Action pattern (e.g., 'read', 'write', 'delete', '*')
    action TEXT NOT NULL DEFAULT '*',
    -- CEL expression for additional conditions
    condition TEXT NOT NULL,
    -- Policy effect: 'allow' or 'deny'
    effect TEXT NOT NULL DEFAULT 'deny' CHECK (effect IN ('allow', 'deny')),
    -- Higher priority = evaluated first (descending order)
    priority INTEGER NOT NULL DEFAULT 0,
    -- Whether this policy is active
    enabled INTEGER NOT NULL DEFAULT 1,
    -- Version number (incremented on each update for optimistic locking)
    version INTEGER NOT NULL DEFAULT 1,
    created_at TEXT NOT NULL DEFAULT (datetime('now')),
    updated_at TEXT NOT NULL DEFAULT (datetime('now')),
    -- Soft delete timestamp (NULL = active, set = deleted)
    deleted_at TEXT
);

CREATE INDEX IF NOT EXISTS idx_org_rbac_policies_org_id ON org_rbac_policies(org_id);
-- Partial index for enabled policies (most queries filter by enabled = 1 and not deleted)
CREATE INDEX IF NOT EXISTS idx_org_rbac_policies_enabled ON org_rbac_policies(org_id, enabled) WHERE enabled = 1 AND deleted_at IS NULL;
-- Index for priority-ordered evaluation
CREATE INDEX IF NOT EXISTS idx_org_rbac_policies_priority ON org_rbac_policies(org_id, priority DESC);
-- Partial unique index: policy names must be unique within an org among non-deleted policies
CREATE UNIQUE INDEX IF NOT EXISTS idx_org_rbac_policies_org_name_active ON org_rbac_policies(org_id, name) WHERE deleted_at IS NULL;
-- Partial index for non-deleted policies (query optimization)
CREATE INDEX IF NOT EXISTS idx_org_rbac_policies_org_active ON org_rbac_policies(org_id) WHERE deleted_at IS NULL;

-- ======================================================================
-- Organization RBAC Policy Versions
-- ======================================================================

-- Version history for org RBAC policies (for audit and rollback).
-- Every update to a policy creates a new version record.
CREATE TABLE IF NOT EXISTS org_rbac_policy_versions (
    id TEXT PRIMARY KEY NOT NULL,
    policy_id TEXT NOT NULL REFERENCES org_rbac_policies(id) ON DELETE CASCADE,
    -- Who created this version (null if system/migration)
    created_by TEXT REFERENCES users(id) ON DELETE SET NULL,
    -- Version number (matches the policy's version at time of creation)
    version INTEGER NOT NULL,
    -- Snapshot of policy fields at this version
    name TEXT NOT NULL,
    description TEXT,
    resource TEXT NOT NULL,
    action TEXT NOT NULL,
    condition TEXT NOT NULL,
    effect TEXT NOT NULL,
    priority INTEGER NOT NULL,
    enabled INTEGER NOT NULL,
    -- Reason for the change (e.g., "Updated condition to include new team")
    reason TEXT,
    -- When this version was created
    created_at TEXT NOT NULL DEFAULT (datetime('now')),
    -- Each version number must be unique per policy
    UNIQUE(policy_id, version)
);

CREATE INDEX IF NOT EXISTS idx_org_rbac_policy_versions_policy_id ON org_rbac_policy_versions(policy_id);
CREATE INDEX IF NOT EXISTS idx_org_rbac_policy_versions_created_by ON org_rbac_policy_versions(created_by);
-- Index for fetching latest version efficiently
CREATE INDEX IF NOT EXISTS idx_org_rbac_policy_versions_latest ON org_rbac_policy_versions(policy_id, version DESC);
-- Index for cleanup jobs finding old versions by creation date
CREATE INDEX IF NOT EXISTS idx_org_rbac_policy_versions_cleanup ON org_rbac_policy_versions(policy_id, created_at);

-- ======================================================================
-- API Keys
-- ======================================================================

-- Can belong to org, team, project, user, or service_account.
-- owner_type: 'organization', 'team', 'project', 'user', 'service_account'
-- budget_period: 'daily', 'monthly'
CREATE TABLE IF NOT EXISTS api_keys (
    id TEXT PRIMARY KEY NOT NULL,
    owner_type TEXT NOT NULL CHECK (owner_type IN ('organization', 'team', 'project', 'user', 'service_account')),
    owner_id TEXT NOT NULL,
    -- Key rotation tracking
    rotated_from_key_id TEXT REFERENCES api_keys(id) ON DELETE SET NULL,
    name TEXT NOT NULL,
    key_hash TEXT NOT NULL UNIQUE,
    key_prefix TEXT NOT NULL,
    -- Budget enforcement
    budget_amount INTEGER,
    budget_period TEXT CHECK (budget_period IN ('daily', 'monthly')),
    -- Permission scopes (JSON array, e.g., ["chat", "embeddings"]; null = no restriction)
    scopes TEXT,
    -- Model patterns (JSON array, e.g., ["gpt-4*", "claude-3-opus"]; null = no restriction)
    allowed_models TEXT,
    -- CIDR blocks (JSON array, e.g., ["10.0.0.0/8"]; null = no restriction)
    ip_allowlist TEXT,
    -- Per-key rate limit overrides (null = use global defaults)
    rate_limit_rpm INTEGER,
    rate_limit_tpm INTEGER,
    -- Sovereignty requirements (data residency constraints for this key)
    sovereignty_requirements TEXT,
    -- Status timestamps
    revoked_at TEXT,
    expires_at TEXT,
    last_used_at TEXT,
    rotation_grace_until TEXT,
    created_at TEXT NOT NULL DEFAULT (datetime('now')),
    updated_at TEXT NOT NULL DEFAULT (datetime('now'))
);

CREATE INDEX IF NOT EXISTS idx_api_keys_key_hash ON api_keys(key_hash);
CREATE INDEX IF NOT EXISTS idx_api_keys_owner ON api_keys(owner_type, owner_id);
CREATE INDEX IF NOT EXISTS idx_api_keys_prefix ON api_keys(key_prefix);
-- Partial index for active (non-revoked) keys - used in authentication hot path
CREATE INDEX IF NOT EXISTS idx_api_keys_active ON api_keys(key_hash) WHERE revoked_at IS NULL;
CREATE INDEX IF NOT EXISTS idx_api_keys_owner_active ON api_keys(owner_type, owner_id) WHERE revoked_at IS NULL;
-- Partial index for service account-owned API keys (used when deleting service accounts)
CREATE INDEX IF NOT EXISTS idx_api_keys_service_account_owner ON api_keys(owner_type, owner_id) WHERE owner_type = 'service_account';

-- ======================================================================
-- Dynamic Providers
-- ======================================================================

-- Org, team, project, or user can define custom LLM providers.
-- owner_type: 'organization', 'team', 'project', 'user'
CREATE TABLE IF NOT EXISTS dynamic_providers (
    id TEXT PRIMARY KEY NOT NULL,
    owner_type TEXT NOT NULL CHECK (owner_type IN ('organization', 'team', 'project', 'user')),
    owner_id TEXT NOT NULL,
    name TEXT NOT NULL,
    provider_type TEXT NOT NULL,
    base_url TEXT NOT NULL DEFAULT '',
    -- Secret manager reference for the API key
    api_key_secret_ref TEXT,
    -- Provider-specific configuration (JSON)
    config TEXT,
    -- Supported models (JSON array)
    models TEXT NOT NULL DEFAULT '[]',
    -- Sovereignty metadata (data residency, compliance requirements)
    sovereignty TEXT,
    is_enabled INTEGER NOT NULL DEFAULT 1,
    created_at TEXT NOT NULL DEFAULT (datetime('now')),
    updated_at TEXT NOT NULL DEFAULT (datetime('now')),
    UNIQUE(owner_type, owner_id, name)
);

CREATE INDEX IF NOT EXISTS idx_dynamic_providers_owner ON dynamic_providers(owner_type, owner_id);

-- ======================================================================
-- Usage Records
-- ======================================================================

-- Tracks request usage with principal-based attribution
CREATE TABLE IF NOT EXISTS usage_records (
    id TEXT PRIMARY KEY NOT NULL,
    -- Unique request identifier for idempotency (prevents duplicate charges)
    request_id TEXT NOT NULL UNIQUE,
    -- Attribution context: nullable to support session-based users without API keys
    api_key_id TEXT REFERENCES api_keys(id) ON DELETE SET NULL,
    -- Principal-based attribution fields
    user_id TEXT,
    org_id TEXT,
    project_id TEXT,
    team_id TEXT,
    service_account_id TEXT,
    model TEXT NOT NULL,
    provider TEXT NOT NULL,
    -- Token counts
    input_tokens INTEGER NOT NULL DEFAULT 0,
    output_tokens INTEGER NOT NULL DEFAULT 0,
    total_tokens INTEGER NOT NULL DEFAULT 0,
    cached_tokens INTEGER NOT NULL DEFAULT 0,
    reasoning_tokens INTEGER NOT NULL DEFAULT 0,
    -- Cost in microcents (1/1,000,000 of a dollar) for sub-cent precision
    cost_microcents INTEGER NOT NULL DEFAULT 0,
    -- Media counts
    image_count INTEGER,
    audio_seconds INTEGER,
    character_count INTEGER,
    -- Request metadata
    streamed INTEGER NOT NULL DEFAULT 0,
    finish_reason TEXT,
    latency_ms INTEGER,
    cancelled INTEGER NOT NULL DEFAULT 0,
    status_code INTEGER,
    pricing_source TEXT NOT NULL DEFAULT 'none',
    provider_source TEXT,
    http_referer TEXT,
    recorded_at TEXT NOT NULL DEFAULT (datetime('now')),
    -- Record type: 'model' for LLM requests, 'tool' for tool invocations
    record_type TEXT NOT NULL DEFAULT 'model',
    -- Tool-specific fields (only populated for record_type='tool')
    tool_name TEXT,
    tool_query TEXT,
    tool_url TEXT,
    tool_bytes_fetched INTEGER,
    tool_results_count INTEGER,
    -- Wall-clock runtime in seconds (only populated for shell tool records)
    tool_runtime_seconds REAL,
    -- Shell process exit code (only populated for shell tool records).
    -- Kept separate from status_code (HTTP) so a shell that exits non-zero
    -- inside a 200 response is observable in usage queries.
    tool_exit_code INTEGER
);

-- SQLite doesn't support partial indexes; use regular indexes
CREATE INDEX IF NOT EXISTS idx_usage_records_api_key_id ON usage_records(api_key_id);
CREATE INDEX IF NOT EXISTS idx_usage_records_api_key_date ON usage_records(api_key_id, recorded_at);
CREATE INDEX IF NOT EXISTS idx_usage_records_api_key_model ON usage_records(api_key_id, model);
CREATE INDEX IF NOT EXISTS idx_usage_records_api_key_date_desc ON usage_records(api_key_id, recorded_at DESC);
CREATE INDEX IF NOT EXISTS idx_usage_records_org_date ON usage_records(org_id, recorded_at);
CREATE INDEX IF NOT EXISTS idx_usage_records_user_date ON usage_records(user_id, recorded_at);
CREATE INDEX IF NOT EXISTS idx_usage_records_project_date ON usage_records(project_id, recorded_at);
CREATE INDEX IF NOT EXISTS idx_usage_records_team_date ON usage_records(team_id, recorded_at);
CREATE INDEX IF NOT EXISTS idx_usage_records_recorded_at ON usage_records(recorded_at);
CREATE INDEX IF NOT EXISTS idx_usage_records_recorded_at_id ON usage_records(recorded_at DESC, id DESC);
CREATE INDEX IF NOT EXISTS idx_usage_records_model ON usage_records(model);
CREATE INDEX IF NOT EXISTS idx_usage_records_request_id ON usage_records(request_id);

-- ======================================================================
-- Model Pricing
-- ======================================================================

-- Per-scope model pricing configuration.
-- Pricing is looked up in order: user -> project -> organization -> static config -> defaults.
-- owner_type: 'organization', 'team', 'project', 'user', or NULL for static/global pricing
CREATE TABLE IF NOT EXISTS model_pricing (
    id TEXT PRIMARY KEY NOT NULL,
    owner_type TEXT CHECK (owner_type IN ('organization', 'team', 'project', 'user') OR owner_type IS NULL),
    owner_id TEXT,
    provider TEXT NOT NULL,
    model TEXT NOT NULL,
    -- All costs in microcents per 1M tokens (divide by 10000 for cents)
    input_per_1m_tokens INTEGER NOT NULL DEFAULT 0,
    output_per_1m_tokens INTEGER NOT NULL DEFAULT 0,
    cached_input_per_1m_tokens INTEGER,
    cache_write_per_1m_tokens INTEGER,
    reasoning_per_1m_tokens INTEGER,
    per_image INTEGER,
    per_request INTEGER,
    -- Per-second pricing for audio transcription/translation (microcents/sec)
    per_second INTEGER,
    -- Per-character pricing for TTS (microcents per 1M characters)
    per_1m_characters INTEGER,
    -- Source of this pricing: 'manual', 'provider_api', 'default'
    source TEXT NOT NULL DEFAULT 'manual',
    created_at TEXT NOT NULL DEFAULT (datetime('now')),
    updated_at TEXT NOT NULL DEFAULT (datetime('now'))
    -- Note: Uniqueness is enforced via partial indexes below (not table-level UNIQUE)
    -- because SQLite treats NULL as distinct in UNIQUE constraints
);

CREATE INDEX IF NOT EXISTS idx_model_pricing_owner ON model_pricing(owner_type, owner_id);
CREATE INDEX IF NOT EXISTS idx_model_pricing_provider_model ON model_pricing(provider, model);
CREATE INDEX IF NOT EXISTS idx_model_pricing_owner_provider ON model_pricing(owner_type, owner_id, provider);
-- Global pricing: unique on (provider, model) when owner is NULL
-- This handles SQLite's NULL distinctness in UNIQUE constraints
CREATE UNIQUE INDEX IF NOT EXISTS idx_model_pricing_unique_global
    ON model_pricing(provider, model) WHERE owner_type IS NULL;
-- Scoped pricing: unique on (owner_type, owner_id, provider, model) when owner is set
CREATE UNIQUE INDEX IF NOT EXISTS idx_model_pricing_unique_scoped
    ON model_pricing(owner_type, owner_id, provider, model) WHERE owner_type IS NOT NULL;

-- ======================================================================
-- Dead Letter Queue
-- ======================================================================

-- Stores failed operations (e.g., usage logging) for later recovery or inspection
CREATE TABLE IF NOT EXISTS dead_letter_queue (
    id TEXT PRIMARY KEY NOT NULL,
    entry_type TEXT NOT NULL,
    payload TEXT NOT NULL,
    error TEXT NOT NULL,
    -- Metadata (JSON)
    metadata TEXT NOT NULL DEFAULT '{}',
    retry_count INTEGER NOT NULL DEFAULT 0,
    created_at TEXT NOT NULL DEFAULT (datetime('now')),
    last_retry_at TEXT
);

CREATE INDEX IF NOT EXISTS idx_dlq_entry_type ON dead_letter_queue(entry_type);
CREATE INDEX IF NOT EXISTS idx_dlq_created_at ON dead_letter_queue(created_at);
CREATE INDEX IF NOT EXISTS idx_dlq_retry_count ON dead_letter_queue(retry_count);

-- ======================================================================
-- Conversations
-- ======================================================================

-- Chat message history storage.
-- owner_type: 'project' or 'user'
-- pin_order: NULL = not pinned, 0-N = pinned with order (lower = higher in list)
CREATE TABLE IF NOT EXISTS conversations (
    id TEXT PRIMARY KEY NOT NULL,
    owner_type TEXT NOT NULL CHECK (owner_type IN ('project', 'user')),
    owner_id TEXT NOT NULL,
    title TEXT NOT NULL,
    -- Model configuration (JSON array)
    models TEXT NOT NULL DEFAULT '[]',
    -- Message history (JSON array)
    messages TEXT NOT NULL DEFAULT '[]',
    pin_order INTEGER,
    created_at TEXT NOT NULL DEFAULT (datetime('now')),
    updated_at TEXT NOT NULL DEFAULT (datetime('now')),
    deleted_at TEXT
);

CREATE INDEX IF NOT EXISTS idx_conversations_owner ON conversations(owner_type, owner_id);
CREATE INDEX IF NOT EXISTS idx_conversations_created_at ON conversations(created_at);
-- Partial index for non-deleted conversations (most queries filter by deleted_at IS NULL)
CREATE INDEX IF NOT EXISTS idx_conversations_owner_active ON conversations(owner_type, owner_id) WHERE deleted_at IS NULL;
-- Index for pinned conversations (for efficient pinned queries per owner)
CREATE INDEX IF NOT EXISTS idx_conversations_owner_pinned ON conversations(owner_type, owner_id, pin_order) WHERE pin_order IS NOT NULL AND deleted_at IS NULL;

-- ======================================================================
-- Audit Logs
-- ======================================================================

-- Tracks admin operations for compliance and debugging
CREATE TABLE IF NOT EXISTS audit_logs (
    id TEXT PRIMARY KEY NOT NULL,
    -- Who performed the action: 'user', 'api_key', 'system'
    actor_type TEXT NOT NULL CHECK (actor_type IN ('user', 'api_key', 'system')),
    -- ID of the actor (user_id or api_key_id, NULL for system)
    actor_id TEXT,
    -- The action performed (e.g., 'api_key.create', 'user.update')
    action TEXT NOT NULL,
    -- Type of resource affected (e.g., 'api_key', 'user', 'organization')
    resource_type TEXT NOT NULL,
    -- ID of the affected resource
    resource_id TEXT NOT NULL,
    -- Optional organization context
    org_id TEXT REFERENCES organizations(id) ON DELETE SET NULL,
    -- Optional project context
    project_id TEXT REFERENCES projects(id) ON DELETE SET NULL,
    -- JSON with additional details (request info, before/after values, etc.)
    details TEXT NOT NULL DEFAULT '{}',
    -- Client IP address
    ip_address TEXT,
    -- Client user agent
    user_agent TEXT,
    -- When the action occurred
    timestamp TEXT NOT NULL DEFAULT (datetime('now'))
);

CREATE INDEX IF NOT EXISTS idx_audit_logs_timestamp ON audit_logs(timestamp);
CREATE INDEX IF NOT EXISTS idx_audit_logs_actor ON audit_logs(actor_type, actor_id);
CREATE INDEX IF NOT EXISTS idx_audit_logs_action ON audit_logs(action);
CREATE INDEX IF NOT EXISTS idx_audit_logs_resource ON audit_logs(resource_type, resource_id);
CREATE INDEX IF NOT EXISTS idx_audit_logs_org_id ON audit_logs(org_id);
CREATE INDEX IF NOT EXISTS idx_audit_logs_project_id ON audit_logs(project_id);
-- Composite index for common filter pattern: action + resource_type
CREATE INDEX IF NOT EXISTS idx_audit_logs_action_resource ON audit_logs(action, resource_type);
CREATE INDEX IF NOT EXISTS idx_audit_logs_org_action_time ON audit_logs(org_id, action, timestamp DESC);

-- ======================================================================
-- Files
-- ======================================================================

-- OpenAI Files API - stores uploaded files before they're added to vector stores.
-- purpose: 'assistants', 'batch', 'fine-tune', 'vision'
-- status: 'uploaded', 'processed', 'error'
CREATE TABLE IF NOT EXISTS files (
    id TEXT PRIMARY KEY NOT NULL,
    -- Ownership (who can access this file)
    owner_type TEXT NOT NULL CHECK (owner_type IN ('organization', 'team', 'project', 'user')),
    owner_id TEXT NOT NULL,
    -- File metadata
    filename TEXT NOT NULL,
    purpose TEXT NOT NULL DEFAULT 'assistants' CHECK (purpose IN ('assistants', 'batch', 'fine-tune', 'vision')),
    content_type TEXT,
    size_bytes INTEGER NOT NULL,
    -- SHA-256 hash of file content for deduplication (64 hex characters)
    content_hash TEXT,
    -- Processing status
    status TEXT NOT NULL DEFAULT 'uploaded' CHECK (status IN ('uploaded', 'processed', 'error')),
    status_details TEXT,
    -- Storage
    storage_backend TEXT NOT NULL DEFAULT 'database' CHECK (storage_backend IN ('database', 'filesystem', 's3')),
    file_data BLOB,
    storage_path TEXT,
    created_at TEXT NOT NULL DEFAULT (datetime('now')),
    expires_at TEXT
);

CREATE INDEX IF NOT EXISTS idx_files_owner ON files(owner_type, owner_id);
CREATE INDEX IF NOT EXISTS idx_files_purpose ON files(purpose);
CREATE INDEX IF NOT EXISTS idx_files_status ON files(status);
-- Index for content hash lookups (deduplication queries)
CREATE INDEX IF NOT EXISTS idx_files_content_hash ON files(content_hash) WHERE content_hash IS NOT NULL;

-- ======================================================================
-- Vector Stores
-- ======================================================================

-- Vector stores for RAG. Follows OpenAI VectorStore schema with multi-tenant ownership.
-- owner_type: 'organization', 'team', 'project', 'user'
-- status: 'in_progress', 'completed', 'expired'
CREATE TABLE IF NOT EXISTS vector_stores (
    id TEXT PRIMARY KEY NOT NULL,
    -- Ownership (who can access this vector store)
    owner_type TEXT NOT NULL CHECK (owner_type IN ('organization', 'team', 'project', 'user')),
    owner_id TEXT NOT NULL,
    name TEXT NOT NULL,
    description TEXT,
    -- Embedding configuration (set at creation, immutable)
    embedding_model TEXT NOT NULL DEFAULT 'text-embedding-3-small',
    embedding_dimensions INTEGER NOT NULL DEFAULT 1536,
    status TEXT NOT NULL DEFAULT 'completed' CHECK (status IN ('in_progress', 'completed', 'expired')),
    -- Usage statistics
    usage_bytes INTEGER NOT NULL DEFAULT 0,
    -- File counts as JSON: {"cancelled":0, "completed":0, "failed":0, "in_progress":0, "total":0}
    file_counts TEXT NOT NULL DEFAULT '{"cancelled":0,"completed":0,"failed":0,"in_progress":0,"total":0}',
    -- Custom metadata (up to 16 key-value pairs, OpenAI-compatible)
    metadata TEXT,
    -- Expiration policy: {"anchor": "last_active_at", "days": N}
    expires_after TEXT,
    expires_at TEXT,
    last_active_at TEXT,
    created_at TEXT NOT NULL DEFAULT (datetime('now')),
    updated_at TEXT NOT NULL DEFAULT (datetime('now')),
    deleted_at TEXT,
    -- Unique name per owner
    UNIQUE(owner_type, owner_id, name)
);

CREATE INDEX IF NOT EXISTS idx_vector_stores_owner ON vector_stores(owner_type, owner_id);
-- Partial index for non-deleted vector_stores (most queries filter by deleted_at IS NULL)
CREATE INDEX IF NOT EXISTS idx_vector_stores_owner_active ON vector_stores(owner_type, owner_id) WHERE deleted_at IS NULL;
CREATE INDEX IF NOT EXISTS idx_vector_stores_status ON vector_stores(status);
CREATE INDEX IF NOT EXISTS idx_vector_stores_expires_at ON vector_stores(expires_at) WHERE expires_at IS NOT NULL;
CREATE INDEX IF NOT EXISTS idx_vector_stores_embedding_model ON vector_stores(embedding_model);

-- ======================================================================
-- Vector Store Files
-- ======================================================================

-- Links files to vector stores. Follows OpenAI VectorStoreFile schema.
-- status: 'in_progress', 'completed', 'cancelled', 'failed'
CREATE TABLE IF NOT EXISTS vector_store_files (
    id TEXT PRIMARY KEY NOT NULL,
    vector_store_id TEXT NOT NULL REFERENCES vector_stores(id) ON DELETE CASCADE,
    file_id TEXT NOT NULL REFERENCES files(id),
    -- Processing status
    status TEXT NOT NULL DEFAULT 'in_progress' CHECK (status IN ('in_progress', 'completed', 'cancelled', 'failed')),
    -- Processing statistics
    usage_bytes INTEGER NOT NULL DEFAULT 0,
    -- Error information (if status = failed): {"code": "string", "message": "string"}
    last_error TEXT,
    -- Chunking strategy: {"type": "auto"|"static", "static": {"max_chunk_size_tokens": N, "chunk_overlap_tokens": N}}
    chunking_strategy TEXT,
    -- Custom attributes for filtering (up to 16 key-value pairs)
    attributes TEXT,
    created_at TEXT NOT NULL DEFAULT (datetime('now')),
    updated_at TEXT NOT NULL DEFAULT (datetime('now')),
    -- Soft delete timestamp (NULL = not deleted)
    deleted_at TEXT
);

-- A file can only be in a vector store once among *live* entries. Using a
-- partial unique index instead of a plain UNIQUE constraint lets a soft-deleted
-- row coexist with a fresh re-add of the same file.
CREATE UNIQUE INDEX IF NOT EXISTS idx_vector_store_files_unique_live
    ON vector_store_files(vector_store_id, file_id)
    WHERE deleted_at IS NULL;

CREATE INDEX IF NOT EXISTS idx_vector_store_files_vector_store ON vector_store_files(vector_store_id);
CREATE INDEX IF NOT EXISTS idx_vector_store_files_file ON vector_store_files(file_id);
CREATE INDEX IF NOT EXISTS idx_vector_store_files_status ON vector_store_files(status);
CREATE INDEX IF NOT EXISTS idx_vector_store_files_deleted_at ON vector_store_files(deleted_at) WHERE deleted_at IS NOT NULL;

-- Note: Document chunks are stored in the vector database (pgvector or Qdrant),
-- not in the relational database. This enables efficient similarity search
-- without cross-database joins. See VectorStore trait for chunk operations.

-- ======================================================================
-- Templates
-- ======================================================================

-- Reusable system prompt templates.
-- owner_type: 'organization', 'team', 'project', 'user'
CREATE TABLE IF NOT EXISTS templates (
    id TEXT PRIMARY KEY NOT NULL,
    -- Ownership (who can access this template)
    owner_type TEXT NOT NULL CHECK (owner_type IN ('organization', 'team', 'project', 'user')),
    owner_id TEXT NOT NULL,
    name TEXT NOT NULL,
    description TEXT,
    -- The actual prompt content (system message template)
    content TEXT NOT NULL,
    -- Optional metadata (temperature, max_tokens, etc.)
    metadata TEXT,
    created_at TEXT NOT NULL DEFAULT (datetime('now')),
    updated_at TEXT NOT NULL DEFAULT (datetime('now')),
    deleted_at TEXT,
    -- Unique name per owner
    UNIQUE(owner_type, owner_id, name)
);

CREATE INDEX IF NOT EXISTS idx_templates_owner ON templates(owner_type, owner_id);
-- Partial index for non-deleted templates (most queries filter by deleted_at IS NULL)
CREATE INDEX IF NOT EXISTS idx_templates_owner_active ON templates(owner_type, owner_id) WHERE deleted_at IS NULL;
CREATE INDEX IF NOT EXISTS idx_templates_name ON templates(name);

-- ======================================================================
-- Service Accounts
-- ======================================================================

-- First-class machine identities that can own API keys and carry roles for
-- RBAC evaluation. Enables unified authorization across human users and
-- machine identities.
CREATE TABLE IF NOT EXISTS service_accounts (
    id TEXT PRIMARY KEY NOT NULL,
    org_id TEXT NOT NULL REFERENCES organizations(id) ON DELETE CASCADE,
    slug TEXT NOT NULL,
    name TEXT NOT NULL,
    description TEXT,
    -- JSON array of role strings (e.g., '["admin", "developer"]')
    -- These roles flow into the RBAC Subject when authenticating via API key
    roles TEXT NOT NULL DEFAULT '[]',
    created_at TEXT NOT NULL DEFAULT (datetime('now')),
    updated_at TEXT NOT NULL DEFAULT (datetime('now')),
    deleted_at TEXT,
    UNIQUE(org_id, slug)
);

CREATE INDEX IF NOT EXISTS idx_service_accounts_org_id ON service_accounts(org_id);
CREATE INDEX IF NOT EXISTS idx_service_accounts_slug ON service_accounts(slug);
-- Partial indexes for non-deleted service accounts (most queries filter by deleted_at IS NULL)
CREATE INDEX IF NOT EXISTS idx_service_accounts_org_active ON service_accounts(org_id) WHERE deleted_at IS NULL;
CREATE INDEX IF NOT EXISTS idx_service_accounts_org_slug_active ON service_accounts(org_id, slug) WHERE deleted_at IS NULL;

-- ======================================================================
-- Skills
-- ======================================================================

-- Agent Skills (https://agentskills.io/specification.md). A skill is a
-- packaged set of instructions (SKILL.md) plus optional bundled files
-- (scripts, references, assets) that the model can auto-invoke or the
-- user can invoke via slash-command / button.
--
-- Files are stored inline in skill_files with a per-skill total size cap
-- enforced in the service layer (config: limits.resource_limits.max_skill_bytes).
CREATE TABLE IF NOT EXISTS skills (
    id TEXT PRIMARY KEY NOT NULL,
    owner_type TEXT NOT NULL CHECK (owner_type IN ('organization', 'team', 'project', 'user')),
    owner_id TEXT NOT NULL,
    -- Per spec: 1..=64 chars, [a-z0-9-]+, no leading/trailing/consecutive hyphens
    name TEXT NOT NULL,
    -- Per spec: required, 1..=1024 chars
    description TEXT NOT NULL,
    -- Optional frontmatter fields (NULL = not set)
    user_invocable INTEGER,                     -- bool (0/1); defaults to true in code
    disable_model_invocation INTEGER,           -- bool (0/1); defaults to false in code
    allowed_tools TEXT,                         -- JSON array of tool names
    argument_hint TEXT,
    source_url TEXT,                            -- origin URL (e.g. GitHub) if imported
    source_ref TEXT,                            -- git ref if imported
    frontmatter_extra TEXT,                     -- JSON object, unknown/forward-compat keys
    -- Cached sum of skill_files.byte_size for fast limit checks
    total_bytes INTEGER NOT NULL DEFAULT 0,
    created_at TEXT NOT NULL DEFAULT (datetime('now')),
    updated_at TEXT NOT NULL DEFAULT (datetime('now')),
    deleted_at TEXT,
    UNIQUE(owner_type, owner_id, name)
);

CREATE INDEX IF NOT EXISTS idx_skills_owner ON skills(owner_type, owner_id);
-- Partial index for non-deleted skills (most queries filter by deleted_at IS NULL)
CREATE INDEX IF NOT EXISTS idx_skills_owner_active ON skills(owner_type, owner_id) WHERE deleted_at IS NULL;
CREATE INDEX IF NOT EXISTS idx_skills_name ON skills(name);

-- Files bundled into a skill. Every skill must have exactly one row with
-- path = 'SKILL.md' (enforced in service layer). Additional rows hold
-- bundled scripts/references/assets referenced from SKILL.md.
CREATE TABLE IF NOT EXISTS skill_files (
    skill_id TEXT NOT NULL REFERENCES skills(id) ON DELETE CASCADE,
    -- Relative path inside the skill directory (e.g. 'SKILL.md', 'scripts/extract.py')
    path TEXT NOT NULL,
    content TEXT NOT NULL,
    -- Cached byte length of content for fast total-size aggregation
    byte_size INTEGER NOT NULL,
    -- MIME type; defaults to 'text/markdown' for SKILL.md, sniffed from
    -- extension for others
    content_type TEXT NOT NULL,
    created_at TEXT NOT NULL DEFAULT (datetime('now')),
    updated_at TEXT NOT NULL DEFAULT (datetime('now')),
    PRIMARY KEY(skill_id, path)
);

CREATE INDEX IF NOT EXISTS idx_skill_files_skill ON skill_files(skill_id);

-- ======================================================================
-- OAuth PKCE Authorization Codes
-- ======================================================================

-- Short-lived, single-use codes issued when a user grants consent on the
-- /oauth/authorize page. The external app exchanges the code (plus its
-- code_verifier) at /oauth/token to receive a user-scoped API key.
--
-- Codes are bound to a single user, callback URL, and PKCE challenge. The
-- `used_at` column is set atomically on exchange to prevent replay.
CREATE TABLE IF NOT EXISTS oauth_authorization_codes (
    id TEXT PRIMARY KEY NOT NULL,
    -- Random opaque code returned to the external app via the callback URL
    code TEXT NOT NULL UNIQUE,
    -- PKCE challenge supplied by the external app
    code_challenge TEXT NOT NULL,
    code_challenge_method TEXT NOT NULL CHECK (code_challenge_method IN ('S256', 'plain')),
    -- Where the user is sent after granting consent; the external app must use
    -- the exact same callback URL when redeeming the code
    callback_url TEXT NOT NULL,
    -- The user who granted consent
    user_id TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    -- Optional human-readable identifier displayed on the consent screen
    app_name TEXT,
    -- The user's choices on the consent page (label, budget, scopes, model
    -- restrictions, etc.) — applied to the issued key on exchange. Stored
    -- as JSON so we can extend the option set without a migration.
    key_options TEXT NOT NULL DEFAULT '{}',
    expires_at TEXT NOT NULL,
    used_at TEXT,
    created_at TEXT NOT NULL DEFAULT (datetime('now'))
);

CREATE INDEX IF NOT EXISTS idx_oauth_authz_codes_code ON oauth_authorization_codes(code);
CREATE INDEX IF NOT EXISTS idx_oauth_authz_codes_user ON oauth_authorization_codes(user_id);
-- Used by the periodic cleanup query to find expired/consumed codes
CREATE INDEX IF NOT EXISTS idx_oauth_authz_codes_expires ON oauth_authorization_codes(expires_at);

-- ======================================================================
-- Responses (Responses API persistence)
-- ======================================================================

-- A persisted response from the Responses API. Stored when the
-- client sends `store=true` (default per OpenAI spec). Allows clients
-- to retrieve responses by ID, list their history, cancel in-progress
-- background responses, and delete persisted records.
--
-- Status lifecycle: queued -> in_progress -> {completed | failed |
-- cancelled | incomplete}.
--
-- `retention_expires_at` drives the cleanup worker that prunes old
-- records (default 24h after the response reaches a terminal state).
-- `request_payload`, `output`, `usage`, `error` are stored as JSON
-- text — the schema is intentionally opaque so the API surface can
-- evolve without further migrations.
CREATE TABLE IF NOT EXISTS responses (
    id TEXT PRIMARY KEY,
    -- Tenancy. Org is required: anonymous-mode deployments still set a
    -- synthetic default org via the auth middleware, so a NULL here
    -- would indicate a bypass we want to reject at insert time.
    org_id TEXT NOT NULL REFERENCES organizations(id) ON DELETE CASCADE,
    -- Ownership. Matches the `skills` / `templates` / `conversations`
    -- pattern — records which scope a response belongs to so it can
    -- be listed/retrieved as an org/team/project/user/service-account
    -- resource. Reads cascade through the org-scope filter; writes
    -- and deletes follow the same cascade.
    owner_type TEXT NOT NULL CHECK (owner_type IN ('organization','team','project','user','service_account')),
    owner_id TEXT NOT NULL,
    -- Audit columns. These record who actually made the call; they
    -- are distinct from the owner above (an API key bound to a
    -- project can submit a request whose owner is the user, etc.).
    project_id TEXT REFERENCES projects(id) ON DELETE SET NULL,
    user_id TEXT REFERENCES users(id) ON DELETE SET NULL,
    api_key_id TEXT REFERENCES api_keys(id) ON DELETE SET NULL,
    service_account_id TEXT REFERENCES service_accounts(id) ON DELETE SET NULL,
    -- Lifecycle
    status TEXT NOT NULL,
    background INTEGER NOT NULL DEFAULT 0,
    model TEXT NOT NULL,
    provider TEXT,
    created_at TEXT NOT NULL,
    started_at TEXT,
    completed_at TEXT,
    -- Payload + result (JSON)
    request_payload TEXT NOT NULL,
    output TEXT,
    usage TEXT,
    error TEXT,
    -- Retention
    retention_expires_at TEXT NOT NULL,
    -- Highest event sequence_number persisted by the event buffer for
    -- this response. Used by the replay endpoint to detect "no more
    -- events coming" without a separate join.
    last_sequence_number INTEGER NOT NULL DEFAULT 0,
    -- Container the shell-tool session for this response wrote files
    -- into. See Postgres migration for the design rationale.
    container_id TEXT
);

CREATE INDEX IF NOT EXISTS idx_responses_org_status ON responses(org_id, status);
CREATE INDEX IF NOT EXISTS idx_responses_owner_created ON responses(owner_type, owner_id, created_at DESC);
CREATE INDEX IF NOT EXISTS idx_responses_retention ON responses(retention_expires_at);

-- Append-only event log for in-flight + completed responses. Powers
-- `GET /v1/responses/{id}/events?starting_after=N` for clients that
-- need to resume a dropped stream — they replay missed events from the
-- log up to whatever sequence number they had when they disconnected,
-- then continue with live events.
--
-- `sequence_number` is monotonic per response and assigned in the
-- gateway (not derived from the upstream provider) so retries don't
-- create gaps. ON DELETE CASCADE on `response_id` keeps the log in
-- sync with `responses` cleanup. The composite PRIMARY KEY already
-- provides the (response_id, sequence_number) b-tree, so no extra
-- index is needed.
CREATE TABLE IF NOT EXISTS response_events (
    response_id TEXT NOT NULL REFERENCES responses(id) ON DELETE CASCADE,
    sequence_number INTEGER NOT NULL,
    event_type TEXT NOT NULL,
    payload TEXT NOT NULL,
    created_at TEXT NOT NULL,
    PRIMARY KEY (response_id, sequence_number)
);

-- ======================================================================
-- Containers (shell-tool `/mnt/data` artifact persistence)
-- ======================================================================

-- Mirror of the Postgres `containers` / `container_files` tables. See
-- the Postgres migration for the design rationale. SQLite stores
-- timestamps as TEXT (ISO-8601) — cursor pagination through these
-- tables relies on millisecond-precision `created_at` values, so all
-- inserts must use `truncate_to_millis(Utc::now())`.
CREATE TABLE IF NOT EXISTS containers (
    id TEXT PRIMARY KEY NOT NULL,
    org_id TEXT NOT NULL REFERENCES organizations(id) ON DELETE CASCADE,
    owner_type TEXT NOT NULL CHECK (owner_type IN ('organization', 'team', 'project', 'user', 'service_account')),
    owner_id TEXT NOT NULL,
    status TEXT NOT NULL CHECK (status IN ('active', 'expired', 'deleted')),
    runtime_label TEXT NOT NULL,
    source_response_id TEXT,
    idle_ttl_secs INTEGER NOT NULL,
    last_active_at TEXT NOT NULL,
    created_at TEXT NOT NULL,
    expires_at TEXT,
    -- POST /v1/containers fields (see postgres migration for rationale).
    name TEXT,
    memory_limit_mb INTEGER,
    network_policy_json TEXT,
    skill_ids_json TEXT
);

CREATE INDEX IF NOT EXISTS idx_containers_org_active
    ON containers(org_id, status, last_active_at);
CREATE INDEX IF NOT EXISTS idx_containers_source_response
    ON containers(source_response_id)
    WHERE source_response_id IS NOT NULL;

CREATE TABLE IF NOT EXISTS container_files (
    id TEXT PRIMARY KEY NOT NULL,
    container_id TEXT NOT NULL REFERENCES containers(id) ON DELETE CASCADE,
    org_id TEXT NOT NULL REFERENCES organizations(id) ON DELETE CASCADE,
    path TEXT NOT NULL,
    filename TEXT NOT NULL,
    size_bytes INTEGER NOT NULL,
    content_type TEXT,
    content_hash TEXT NOT NULL,
    source TEXT NOT NULL CHECK (source IN ('user', 'assistant')),
    storage_backend TEXT NOT NULL DEFAULT 'database' CHECK (storage_backend IN ('database', 'filesystem', 's3')),
    file_data BLOB,
    storage_path TEXT,
    source_response_id TEXT,
    source_call_id TEXT,
    created_at TEXT NOT NULL,
    UNIQUE(container_id, path)
);

CREATE INDEX IF NOT EXISTS idx_container_files_container_created
    ON container_files(container_id, created_at DESC);
CREATE INDEX IF NOT EXISTS idx_container_files_org
    ON container_files(org_id);

-- ─────────────────────────────────────────────────────────────────────────────
-- mcp_pending_approvals
-- ─────────────────────────────────────────────────────────────────────────────
-- See the Postgres mirror for full doc. Timestamps as TEXT (ISO 8601
-- with millisecond precision — call `truncate_to_millis(Utc::now())`
-- when writing, per `agent_instructions/database_changes.md`).
CREATE TABLE IF NOT EXISTS mcp_pending_approvals (
    id TEXT PRIMARY KEY NOT NULL,
    response_id TEXT NOT NULL,
    org_id TEXT NOT NULL REFERENCES organizations(id) ON DELETE CASCADE,
    call_id TEXT NOT NULL,
    server_label TEXT NOT NULL,
    server_url TEXT NOT NULL,
    tool_name TEXT NOT NULL,
    arguments_json TEXT NOT NULL,
    created_at TEXT NOT NULL,
    expires_at TEXT NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_mcp_pending_approvals_response
    ON mcp_pending_approvals(response_id);
CREATE INDEX IF NOT EXISTS idx_mcp_pending_approvals_expires
    ON mcp_pending_approvals(expires_at);
