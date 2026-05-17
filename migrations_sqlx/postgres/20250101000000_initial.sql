-- Initial schema for Hadrian Gateway (PostgreSQL)

-- ======================================================================
-- Organizations
-- ======================================================================

CREATE TABLE IF NOT EXISTS organizations (
    id UUID PRIMARY KEY NOT NULL,
    slug VARCHAR(64) NOT NULL UNIQUE,
    name VARCHAR(255) NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    deleted_at TIMESTAMPTZ
);

CREATE INDEX IF NOT EXISTS idx_organizations_slug ON organizations(slug);
-- Partial index for non-deleted organizations (most queries filter by deleted_at IS NULL)
CREATE INDEX IF NOT EXISTS idx_organizations_slug_active ON organizations(slug) WHERE deleted_at IS NULL;

-- ======================================================================
-- Teams
-- ======================================================================

-- Groups within organizations
CREATE TABLE IF NOT EXISTS teams (
    id UUID PRIMARY KEY NOT NULL,
    org_id UUID NOT NULL REFERENCES organizations(id) ON DELETE CASCADE,
    slug VARCHAR(64) NOT NULL,
    name VARCHAR(255) NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    deleted_at TIMESTAMPTZ,
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
    id UUID PRIMARY KEY NOT NULL,
    org_id UUID NOT NULL REFERENCES organizations(id) ON DELETE CASCADE,
    team_id UUID REFERENCES teams(id) ON DELETE SET NULL,
    slug VARCHAR(64) NOT NULL,
    name VARCHAR(255) NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    deleted_at TIMESTAMPTZ,
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
    id UUID PRIMARY KEY NOT NULL,
    external_id VARCHAR(255) NOT NULL UNIQUE,
    email VARCHAR(255) UNIQUE,
    name VARCHAR(255),
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE INDEX IF NOT EXISTS idx_users_external_id ON users(external_id);
CREATE INDEX IF NOT EXISTS idx_users_email ON users(email);

-- ======================================================================
-- Organization Memberships
-- ======================================================================

-- Membership source type (how the membership was created)
DO $$ BEGIN
    CREATE TYPE membership_source AS ENUM ('manual', 'jit', 'scim');
EXCEPTION
    WHEN duplicate_object THEN null;
END $$;

-- Users belong to organizations
CREATE TABLE IF NOT EXISTS org_memberships (
    org_id UUID NOT NULL REFERENCES organizations(id) ON DELETE CASCADE,
    user_id UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    role VARCHAR(32) NOT NULL DEFAULT 'member',
    -- Source of membership: manual (admin/API), jit (SSO login), scim (IdP push)
    source membership_source NOT NULL DEFAULT 'manual',
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
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
CREATE TABLE IF NOT EXISTS project_memberships (
    project_id UUID NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
    user_id UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    role VARCHAR(32) NOT NULL DEFAULT 'member',
    -- Source of membership: manual (admin/API), jit (SSO login), scim (IdP push)
    source membership_source NOT NULL DEFAULT 'manual',
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    PRIMARY KEY (project_id, user_id)
);

CREATE INDEX IF NOT EXISTS idx_project_members_user_id ON project_memberships(user_id);
-- Index for querying memberships by source
CREATE INDEX IF NOT EXISTS idx_project_members_source ON project_memberships(user_id, source);

-- ======================================================================
-- Team Memberships
-- ======================================================================

-- Users belong to teams
CREATE TABLE IF NOT EXISTS team_memberships (
    team_id UUID NOT NULL REFERENCES teams(id) ON DELETE CASCADE,
    user_id UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    role VARCHAR(32) NOT NULL DEFAULT 'member',
    -- Source of membership: manual (admin/API), jit (SSO login), scim (IdP push)
    source membership_source NOT NULL DEFAULT 'manual',
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
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
    id UUID PRIMARY KEY NOT NULL,
    -- Organization context (required - mappings are org-scoped)
    org_id UUID NOT NULL REFERENCES organizations(id) ON DELETE CASCADE,
    -- Optional: Team to add user to when they have this IdP group
    team_id UUID REFERENCES teams(id) ON DELETE CASCADE,
    -- Which SSO connection this mapping applies to (from config)
    sso_connection_name VARCHAR(64) NOT NULL DEFAULT 'default',
    -- The IdP group name (exactly as it appears in the groups claim)
    idp_group VARCHAR(512) NOT NULL,
    -- Optional: Role to assign (within the team if team_id set, otherwise org role)
    role VARCHAR(32),
    -- Priority for role precedence (higher = wins when multiple mappings target same team)
    priority INTEGER NOT NULL DEFAULT 0,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    -- Unique constraint: prevent duplicate mappings (same connection + group + org + team)
    -- NULLS NOT DISTINCT ensures NULL team_id values are treated as equal for uniqueness
    UNIQUE NULLS NOT DISTINCT (sso_connection_name, idp_group, org_id, team_id)
);

-- Index for looking up mappings by SSO connection and org
CREATE INDEX IF NOT EXISTS idx_sso_group_mappings_connection_org ON sso_group_mappings(sso_connection_name, org_id);
-- Index for looking up mappings by IdP group (for resolving user's groups)
CREATE INDEX IF NOT EXISTS idx_sso_group_mappings_idp_group ON sso_group_mappings(idp_group);
-- Index for org-scoped queries
CREATE INDEX IF NOT EXISTS idx_sso_group_mappings_org_id ON sso_group_mappings(org_id);

-- ======================================================================
-- Organization SSO Configurations
-- ======================================================================

-- Per-org OIDC/SAML settings for multi-tenant SSO.
-- Each organization can have its own IdP configuration.
-- When a user logs in, the system routes to the correct IdP based on email domain.

DO $$ BEGIN
    CREATE TYPE sso_provider_type AS ENUM ('oidc', 'saml');
EXCEPTION
    WHEN duplicate_object THEN null;
END $$;

DO $$ BEGIN
    CREATE TYPE sso_enforcement_mode AS ENUM ('optional', 'required', 'test');
EXCEPTION
    WHEN duplicate_object THEN null;
END $$;

CREATE TABLE IF NOT EXISTS org_sso_configs (
    id UUID PRIMARY KEY NOT NULL,
    -- Organization this SSO config belongs to (one SSO config per org)
    org_id UUID NOT NULL UNIQUE REFERENCES organizations(id) ON DELETE CASCADE,
    -- Provider type: 'oidc' or 'saml'
    provider_type sso_provider_type NOT NULL DEFAULT 'oidc',

    -- ==========================================================================
    -- OIDC Configuration (used when provider_type = 'oidc')
    -- ==========================================================================
    -- OIDC issuer URL (e.g., "https://accounts.google.com")
    -- Required for OIDC, NULL for SAML
    issuer VARCHAR(512),
    -- OIDC discovery URL (defaults to issuer/.well-known/openid-configuration)
    discovery_url VARCHAR(512),
    -- OAuth2 client ID (required for OIDC, NULL for SAML)
    client_id VARCHAR(256),
    -- Client secret stored in secret manager, this is the key reference
    -- Required for OIDC, NULL for SAML
    client_secret_key VARCHAR(512),
    -- Redirect URI (optional - can use global default)
    redirect_uri VARCHAR(512),
    -- Scopes as space-separated string (e.g., 'openid email profile groups')
    scopes VARCHAR(512) NOT NULL DEFAULT 'openid email profile',
    -- Claims configuration (OIDC-specific)
    identity_claim VARCHAR(64),
    org_claim VARCHAR(64),
    groups_claim VARCHAR(64),

    -- ==========================================================================
    -- SAML 2.0 Configuration (used when provider_type = 'saml')
    -- ==========================================================================
    -- IdP metadata URL for auto-configuration (alternative to manual config)
    saml_metadata_url VARCHAR(512),
    -- IdP entity identifier (e.g., "https://idp.example.com/metadata")
    saml_idp_entity_id VARCHAR(512),
    -- IdP Single Sign-On service URL (HTTP-Redirect or HTTP-POST binding)
    saml_idp_sso_url VARCHAR(512),
    -- IdP Single Logout service URL (optional)
    saml_idp_slo_url VARCHAR(512),
    -- IdP X.509 certificate for signature validation (PEM format)
    saml_idp_certificate TEXT,
    -- Service Provider entity ID (Hadrian's identifier to the IdP)
    saml_sp_entity_id VARCHAR(512),
    -- NameID format to request (e.g., 'urn:oasis:names:tc:SAML:1.1:nameid-format:emailAddress')
    saml_name_id_format VARCHAR(256),
    -- Whether to sign AuthnRequests
    saml_sign_requests BOOLEAN NOT NULL DEFAULT FALSE,
    -- SP private key reference in secret manager (used for signing requests)
    saml_sp_private_key_ref VARCHAR(512),
    -- SP X.509 certificate for metadata (PEM format, not a secret)
    saml_sp_certificate TEXT,
    -- Whether to force re-authentication at IdP
    saml_force_authn BOOLEAN NOT NULL DEFAULT FALSE,
    -- Requested authentication context class
    saml_authn_context_class_ref VARCHAR(256),
    -- SAML attribute name for user identity (like identity_claim for OIDC)
    saml_identity_attribute VARCHAR(256),
    -- SAML attribute name for email
    saml_email_attribute VARCHAR(256),
    -- SAML attribute name for display name
    saml_name_attribute VARCHAR(256),
    -- SAML attribute name for groups
    saml_groups_attribute VARCHAR(256),

    -- ==========================================================================
    -- JIT Provisioning (shared by OIDC and SAML)
    -- ==========================================================================
    provisioning_enabled BOOLEAN NOT NULL DEFAULT TRUE,
    create_users BOOLEAN NOT NULL DEFAULT TRUE,
    default_team_id UUID REFERENCES teams(id) ON DELETE SET NULL,
    default_org_role VARCHAR(32) NOT NULL DEFAULT 'member',
    default_team_role VARCHAR(32) NOT NULL DEFAULT 'member',
    -- JSON array of allowed email domains (e.g., '["acme.com", "acme.io"]')
    allowed_email_domains JSONB,
    sync_attributes_on_login BOOLEAN NOT NULL DEFAULT FALSE,
    sync_memberships_on_login BOOLEAN NOT NULL DEFAULT TRUE,

    -- ==========================================================================
    -- Status & Enforcement
    -- ==========================================================================
    -- SSO enforcement mode: 'optional' (allow other auth), 'required' (SSO only), 'test' (shadow mode)
    enforcement_mode sso_enforcement_mode NOT NULL DEFAULT 'optional',
    -- Whether this SSO config is active
    enabled BOOLEAN NOT NULL DEFAULT TRUE,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

-- Index for looking up SSO config by org_id (also covered by UNIQUE constraint)
CREATE INDEX IF NOT EXISTS idx_org_sso_configs_org_id ON org_sso_configs(org_id);
-- Index for enabled SSO configs (for IdP discovery)
CREATE INDEX IF NOT EXISTS idx_org_sso_configs_enabled ON org_sso_configs(enabled) WHERE enabled = TRUE;
-- Index for gateway JWT issuer-based lookup (per-org JWT validation hot path)
CREATE INDEX IF NOT EXISTS idx_org_sso_configs_issuer_enabled
  ON org_sso_configs(issuer) WHERE enabled = TRUE AND provider_type = 'oidc'::sso_provider_type;

-- ======================================================================
-- Domain Verifications
-- ======================================================================

-- Verify ownership of email domains for SSO.
-- status: 'pending', 'verified', 'failed'

DO $$ BEGIN
    CREATE TYPE domain_verification_status AS ENUM ('pending', 'verified', 'failed');
EXCEPTION
    WHEN duplicate_object THEN null;
END $$;

CREATE TABLE IF NOT EXISTS domain_verifications (
    id UUID PRIMARY KEY NOT NULL,
    -- SSO config this verification belongs to
    org_sso_config_id UUID NOT NULL REFERENCES org_sso_configs(id) ON DELETE CASCADE,
    -- The domain being verified (e.g., "acme.com")
    domain VARCHAR(255) NOT NULL,
    -- Random token for DNS TXT record verification
    verification_token VARCHAR(64) NOT NULL,
    -- Verification status
    status domain_verification_status NOT NULL DEFAULT 'pending',
    -- The actual DNS TXT record found during verification (for audit)
    dns_txt_record VARCHAR(512),
    -- Number of verification attempts
    verification_attempts INTEGER NOT NULL DEFAULT 0,
    -- Last verification attempt timestamp
    last_attempt_at TIMESTAMPTZ,
    -- When the domain was successfully verified
    verified_at TIMESTAMPTZ,
    -- Optional: require re-verification after this date
    expires_at TIMESTAMPTZ,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
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
    id UUID PRIMARY KEY NOT NULL,
    -- Organization this SCIM config belongs to (one SCIM config per org)
    org_id UUID NOT NULL UNIQUE REFERENCES organizations(id) ON DELETE CASCADE,
    -- Whether SCIM provisioning is enabled
    enabled BOOLEAN NOT NULL DEFAULT true,
    -- Bearer token hash for SCIM API authentication
    token_hash VARCHAR(64) NOT NULL,
    -- Token prefix for identification (first 8 chars, like 'scim_xxxx')
    token_prefix VARCHAR(16) NOT NULL,
    -- Last time the SCIM token was used
    token_last_used_at TIMESTAMPTZ,
    -- Provisioning settings
    create_users BOOLEAN NOT NULL DEFAULT true,
    default_team_id UUID REFERENCES teams(id) ON DELETE SET NULL,
    default_org_role VARCHAR(32) NOT NULL DEFAULT 'member',
    default_team_role VARCHAR(32) NOT NULL DEFAULT 'member',
    -- Whether to sync display name from SCIM
    sync_display_name BOOLEAN NOT NULL DEFAULT true,
    -- Deprovisioning behavior: delete user entirely (false = just deactivate)
    deactivate_deletes_user BOOLEAN NOT NULL DEFAULT false,
    -- Whether to revoke all API keys when user is deactivated via SCIM
    revoke_api_keys_on_deactivate BOOLEAN NOT NULL DEFAULT true,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE INDEX IF NOT EXISTS idx_org_scim_configs_org_id ON org_scim_configs(org_id);
CREATE INDEX IF NOT EXISTS idx_org_scim_configs_enabled ON org_scim_configs(enabled) WHERE enabled = true;
-- Index for token authentication lookups
CREATE INDEX IF NOT EXISTS idx_org_scim_configs_token_prefix ON org_scim_configs(token_prefix);

-- ======================================================================
-- SCIM User Mappings
-- ======================================================================

-- Maps SCIM external IDs to Hadrian user IDs (per-org).
-- Allows the same user to have different SCIM IDs in different orgs
-- and tracks the SCIM-specific "active" state separately from user deletion.
CREATE TABLE IF NOT EXISTS scim_user_mappings (
    id UUID PRIMARY KEY NOT NULL,
    org_id UUID NOT NULL REFERENCES organizations(id) ON DELETE CASCADE,
    -- Hadrian user this maps to
    user_id UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    -- SCIM external ID from IdP (e.g., Okta user ID like '00u1a2b3c4d5e6f7g8h9')
    scim_external_id VARCHAR(255) NOT NULL,
    -- SCIM "active" status (separate from user existence)
    active BOOLEAN NOT NULL DEFAULT true,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
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
    id UUID PRIMARY KEY NOT NULL,
    org_id UUID NOT NULL REFERENCES organizations(id) ON DELETE CASCADE,
    -- Hadrian team this maps to
    team_id UUID NOT NULL REFERENCES teams(id) ON DELETE CASCADE,
    -- SCIM group ID from IdP
    scim_group_id VARCHAR(255) NOT NULL,
    -- Display name from SCIM (for reference)
    display_name VARCHAR(255),
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    -- Each SCIM group can only map to one team per org
    UNIQUE(org_id, scim_group_id)
);

CREATE INDEX IF NOT EXISTS idx_scim_group_mappings_org_id ON scim_group_mappings(org_id);
CREATE INDEX IF NOT EXISTS idx_scim_group_mappings_team_id ON scim_group_mappings(team_id);
CREATE INDEX IF NOT EXISTS idx_scim_group_mappings_scim_group_id ON scim_group_mappings(org_id, scim_group_id);

-- ======================================================================
-- Organization RBAC Policies
-- ======================================================================

-- Policy effect type
DO $$ BEGIN
    CREATE TYPE rbac_policy_effect AS ENUM ('allow', 'deny');
EXCEPTION
    WHEN duplicate_object THEN null;
END $$;

-- Per-organization CEL-based authorization policies for runtime policy management.
-- effect: 'allow' or 'deny' (explicit allow/deny semantic)
-- priority: Higher priority policies are evaluated first (descending order)
CREATE TABLE IF NOT EXISTS org_rbac_policies (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    org_id UUID NOT NULL REFERENCES organizations(id) ON DELETE CASCADE,
    name VARCHAR(128) NOT NULL,
    description TEXT,
    -- Resource pattern (e.g., 'projects/*', 'teams/engineering/*', '*')
    resource VARCHAR(128) NOT NULL DEFAULT '*',
    -- Action pattern (e.g., 'read', 'write', 'delete', '*')
    action VARCHAR(64) NOT NULL DEFAULT '*',
    -- CEL expression for additional conditions
    condition TEXT NOT NULL,
    -- Policy effect: 'allow' or 'deny'
    effect rbac_policy_effect NOT NULL DEFAULT 'deny',
    -- Higher priority = evaluated first (descending order)
    priority INTEGER NOT NULL DEFAULT 0,
    -- Whether this policy is active
    enabled BOOLEAN NOT NULL DEFAULT TRUE,
    -- Version number (incremented on each update for optimistic locking)
    version INTEGER NOT NULL DEFAULT 1,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    -- Soft delete timestamp (NULL = active, set = deleted)
    deleted_at TIMESTAMPTZ
);

CREATE INDEX IF NOT EXISTS idx_org_rbac_policies_org_id ON org_rbac_policies(org_id);
-- Partial index for enabled policies (most queries filter by enabled = true and not deleted)
CREATE INDEX IF NOT EXISTS idx_org_rbac_policies_enabled ON org_rbac_policies(org_id, enabled) WHERE enabled = TRUE AND deleted_at IS NULL;
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
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    policy_id UUID NOT NULL REFERENCES org_rbac_policies(id) ON DELETE CASCADE,
    -- Who created this version (null if system/migration)
    created_by UUID REFERENCES users(id) ON DELETE SET NULL,
    -- Version number (matches the policy's version at time of creation)
    version INTEGER NOT NULL,
    -- Snapshot of policy fields at this version
    name VARCHAR(128) NOT NULL,
    description TEXT,
    resource VARCHAR(128) NOT NULL,
    action VARCHAR(64) NOT NULL,
    condition TEXT NOT NULL,
    effect rbac_policy_effect NOT NULL,
    priority INTEGER NOT NULL,
    enabled BOOLEAN NOT NULL,
    -- Reason for the change (e.g., "Updated condition to include new team")
    reason TEXT,
    -- When this version was created
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
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
DO $$ BEGIN
    CREATE TYPE api_key_owner_type AS ENUM ('organization', 'team', 'project', 'user', 'service_account');
EXCEPTION
    WHEN duplicate_object THEN null;
END $$;

DO $$ BEGIN
    CREATE TYPE budget_period AS ENUM ('daily', 'monthly');
EXCEPTION
    WHEN duplicate_object THEN null;
END $$;

CREATE TABLE IF NOT EXISTS api_keys (
    id UUID PRIMARY KEY NOT NULL,
    owner_type api_key_owner_type NOT NULL,
    owner_id UUID NOT NULL,
    -- Key rotation tracking
    rotated_from_key_id UUID REFERENCES api_keys(id) ON DELETE SET NULL,
    name VARCHAR(255) NOT NULL,
    key_hash VARCHAR(64) NOT NULL UNIQUE,
    key_prefix VARCHAR(16) NOT NULL,
    -- Budget enforcement
    budget_amount BIGINT,
    budget_period budget_period,
    -- Permission scopes (JSON array; null = no restriction)
    scopes JSONB,
    -- Model patterns (JSON array; null = no restriction)
    allowed_models JSONB,
    -- CIDR blocks (JSON array; null = no restriction)
    ip_allowlist JSONB,
    -- Per-key rate limit overrides (null = use global defaults)
    rate_limit_rpm INTEGER,
    rate_limit_tpm INTEGER,
    -- Sovereignty requirements (data residency constraints for this key)
    sovereignty_requirements JSONB,
    -- Status timestamps
    revoked_at TIMESTAMPTZ,
    expires_at TIMESTAMPTZ,
    last_used_at TIMESTAMPTZ,
    rotation_grace_until TIMESTAMPTZ,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE INDEX IF NOT EXISTS idx_api_keys_key_hash ON api_keys(key_hash);
CREATE INDEX IF NOT EXISTS idx_api_keys_owner ON api_keys(owner_type, owner_id);
CREATE INDEX IF NOT EXISTS idx_api_keys_prefix ON api_keys(key_prefix);
-- Partial index for active (non-revoked) keys - used in authentication hot path
CREATE INDEX IF NOT EXISTS idx_api_keys_active ON api_keys(key_hash) WHERE revoked_at IS NULL;
CREATE INDEX IF NOT EXISTS idx_api_keys_owner_active ON api_keys(owner_type, owner_id) WHERE revoked_at IS NULL;
-- Partial index for service account-owned API keys (used when deleting service accounts)
CREATE INDEX IF NOT EXISTS idx_api_keys_service_account_owner ON api_keys(owner_id) WHERE owner_type = 'service_account';

-- ======================================================================
-- Dynamic Providers
-- ======================================================================

-- Org, team, project, or user can define custom LLM providers.
DO $$ BEGIN
    CREATE TYPE dynamic_provider_owner_type AS ENUM ('organization', 'team', 'project', 'user');
EXCEPTION
    WHEN duplicate_object THEN null;
END $$;

CREATE TABLE IF NOT EXISTS dynamic_providers (
    id UUID PRIMARY KEY NOT NULL,
    owner_type dynamic_provider_owner_type NOT NULL,
    owner_id UUID NOT NULL,
    name VARCHAR(64) NOT NULL,
    provider_type VARCHAR(64) NOT NULL,
    base_url TEXT NOT NULL DEFAULT '',
    -- Secret manager reference for the API key
    api_key_secret_ref VARCHAR(255),
    -- Provider-specific configuration (JSON)
    config JSONB,
    -- Supported models (JSON array)
    models JSONB NOT NULL DEFAULT '[]',
    -- Sovereignty metadata (data residency, compliance requirements)
    sovereignty JSONB,
    is_enabled BOOLEAN NOT NULL DEFAULT TRUE,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    UNIQUE(owner_type, owner_id, name)
);

CREATE INDEX IF NOT EXISTS idx_dynamic_providers_owner ON dynamic_providers(owner_type, owner_id);

-- ======================================================================
-- Usage Records
-- ======================================================================

-- Tracks request usage with principal-based attribution
CREATE TABLE IF NOT EXISTS usage_records (
    id UUID PRIMARY KEY NOT NULL,
    -- Unique request identifier for idempotency (prevents duplicate charges)
    request_id TEXT NOT NULL UNIQUE,
    -- Attribution context: nullable to support session-based users without API keys
    api_key_id UUID REFERENCES api_keys(id) ON DELETE SET NULL,
    -- Principal-based attribution fields (all nullable, no FKs to avoid feature-gated table issues)
    user_id UUID,
    org_id UUID,
    project_id UUID,
    team_id UUID,
    service_account_id UUID,
    model VARCHAR(128) NOT NULL,
    provider VARCHAR(64) NOT NULL,
    -- Token counts
    input_tokens INTEGER NOT NULL DEFAULT 0,
    output_tokens INTEGER NOT NULL DEFAULT 0,
    total_tokens INTEGER NOT NULL DEFAULT 0,
    cached_tokens INTEGER NOT NULL DEFAULT 0,
    reasoning_tokens INTEGER NOT NULL DEFAULT 0,
    -- Cost in microcents (1/1,000,000 of a dollar) for sub-cent precision
    cost_microcents BIGINT NOT NULL DEFAULT 0,
    -- Media counts
    image_count INTEGER,
    audio_seconds INTEGER,
    character_count INTEGER,
    -- Request metadata
    streamed BOOLEAN NOT NULL DEFAULT FALSE,
    finish_reason VARCHAR(32),
    latency_ms INTEGER,
    cancelled BOOLEAN NOT NULL DEFAULT FALSE,
    status_code SMALLINT,
    pricing_source VARCHAR(20) NOT NULL DEFAULT 'none',
    provider_source VARCHAR(16),
    http_referer TEXT,
    recorded_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    -- Record type: 'model' for LLM requests, 'tool' for tool invocations
    record_type VARCHAR(16) NOT NULL DEFAULT 'model',
    -- Tool-specific fields (only populated for record_type='tool')
    tool_name VARCHAR(64),
    tool_query TEXT,
    tool_url TEXT,
    tool_bytes_fetched BIGINT,
    tool_results_count INTEGER,
    -- Wall-clock runtime in seconds (only populated for shell tool records)
    tool_runtime_seconds DOUBLE PRECISION
);

-- API key indexes (partial: only index rows with api_key_id)
CREATE INDEX IF NOT EXISTS idx_usage_records_api_key_id ON usage_records(api_key_id) WHERE api_key_id IS NOT NULL;
CREATE INDEX IF NOT EXISTS idx_usage_records_api_key_date ON usage_records(api_key_id, recorded_at) WHERE api_key_id IS NOT NULL;
CREATE INDEX IF NOT EXISTS idx_usage_records_api_key_model ON usage_records(api_key_id, model) WHERE api_key_id IS NOT NULL;
CREATE INDEX IF NOT EXISTS idx_usage_records_api_key_date_desc ON usage_records(api_key_id, recorded_at DESC) WHERE api_key_id IS NOT NULL;
-- Scope-level indexes (partial: only index rows with the relevant scope)
CREATE INDEX IF NOT EXISTS idx_usage_records_org_date ON usage_records(org_id, recorded_at) WHERE org_id IS NOT NULL;
CREATE INDEX IF NOT EXISTS idx_usage_records_user_date ON usage_records(user_id, recorded_at) WHERE user_id IS NOT NULL;
CREATE INDEX IF NOT EXISTS idx_usage_records_project_date ON usage_records(project_id, recorded_at) WHERE project_id IS NOT NULL;
CREATE INDEX IF NOT EXISTS idx_usage_records_team_date ON usage_records(team_id, recorded_at) WHERE team_id IS NOT NULL;
-- General indexes
CREATE INDEX IF NOT EXISTS idx_usage_records_recorded_at ON usage_records(recorded_at);
CREATE INDEX IF NOT EXISTS idx_usage_records_recorded_at_id ON usage_records(recorded_at DESC, id DESC);
CREATE INDEX IF NOT EXISTS idx_usage_records_model ON usage_records(model);
CREATE INDEX IF NOT EXISTS idx_usage_records_request_id ON usage_records(request_id);

-- ======================================================================
-- Model Pricing
-- ======================================================================

-- Per-scope model pricing configuration.
-- Pricing is looked up in order: user -> project -> organization -> static config -> defaults.

DO $$ BEGIN
    CREATE TYPE model_pricing_owner_type AS ENUM ('organization', 'team', 'project', 'user');
EXCEPTION
    WHEN duplicate_object THEN null;
END $$;

DO $$ BEGIN
    CREATE TYPE pricing_source AS ENUM ('manual', 'provider_api', 'default');
EXCEPTION
    WHEN duplicate_object THEN null;
END $$;

CREATE TABLE IF NOT EXISTS model_pricing (
    id UUID PRIMARY KEY NOT NULL,
    -- NULL for global/static pricing
    owner_type model_pricing_owner_type,
    owner_id UUID,
    provider VARCHAR(64) NOT NULL,
    model VARCHAR(128) NOT NULL,
    -- All costs in microcents per 1M tokens (divide by 10000 for cents)
    input_per_1m_tokens BIGINT NOT NULL DEFAULT 0,
    output_per_1m_tokens BIGINT NOT NULL DEFAULT 0,
    cached_input_per_1m_tokens BIGINT,
    cache_write_per_1m_tokens BIGINT,
    reasoning_per_1m_tokens BIGINT,
    per_image BIGINT,
    per_request BIGINT,
    -- Per-second pricing for audio transcription/translation (microcents/sec)
    per_second BIGINT,
    -- Per-character pricing for TTS (microcents per 1M characters)
    per_1m_characters BIGINT,
    source pricing_source NOT NULL DEFAULT 'manual',
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    -- Global pricing (owner_type IS NULL) is unique per provider/model
    -- Scoped pricing is unique per owner_type/owner_id/provider/model
    UNIQUE NULLS NOT DISTINCT (owner_type, owner_id, provider, model)
);

CREATE INDEX IF NOT EXISTS idx_model_pricing_owner ON model_pricing(owner_type, owner_id);
CREATE INDEX IF NOT EXISTS idx_model_pricing_provider_model ON model_pricing(provider, model);
CREATE INDEX IF NOT EXISTS idx_model_pricing_owner_provider ON model_pricing(owner_type, owner_id, provider);

-- ======================================================================
-- Triggers
-- ======================================================================

-- Updated_at trigger function
CREATE OR REPLACE FUNCTION update_updated_at_column()
RETURNS TRIGGER AS $$
BEGIN
    NEW.updated_at = NOW();
    RETURN NEW;
END;
$$ language 'plpgsql';

-- Apply updated_at triggers (using IF NOT EXISTS pattern)
DO $$ BEGIN
    CREATE TRIGGER update_organizations_updated_at BEFORE UPDATE ON organizations FOR EACH ROW EXECUTE FUNCTION update_updated_at_column();
EXCEPTION WHEN duplicate_object THEN null;
END $$;

DO $$ BEGIN
    CREATE TRIGGER update_teams_updated_at BEFORE UPDATE ON teams FOR EACH ROW EXECUTE FUNCTION update_updated_at_column();
EXCEPTION WHEN duplicate_object THEN null;
END $$;

DO $$ BEGIN
    CREATE TRIGGER update_projects_updated_at BEFORE UPDATE ON projects FOR EACH ROW EXECUTE FUNCTION update_updated_at_column();
EXCEPTION WHEN duplicate_object THEN null;
END $$;

DO $$ BEGIN
    CREATE TRIGGER update_users_updated_at BEFORE UPDATE ON users FOR EACH ROW EXECUTE FUNCTION update_updated_at_column();
EXCEPTION WHEN duplicate_object THEN null;
END $$;

DO $$ BEGIN
    CREATE TRIGGER update_api_keys_updated_at BEFORE UPDATE ON api_keys FOR EACH ROW EXECUTE FUNCTION update_updated_at_column();
EXCEPTION WHEN duplicate_object THEN null;
END $$;

DO $$ BEGIN
    CREATE TRIGGER update_sso_group_mappings_updated_at BEFORE UPDATE ON sso_group_mappings FOR EACH ROW EXECUTE FUNCTION update_updated_at_column();
EXCEPTION WHEN duplicate_object THEN null;
END $$;

DO $$ BEGIN
    CREATE TRIGGER update_dynamic_providers_updated_at BEFORE UPDATE ON dynamic_providers FOR EACH ROW EXECUTE FUNCTION update_updated_at_column();
EXCEPTION WHEN duplicate_object THEN null;
END $$;

DO $$ BEGIN
    CREATE TRIGGER update_model_pricing_updated_at BEFORE UPDATE ON model_pricing FOR EACH ROW EXECUTE FUNCTION update_updated_at_column();
EXCEPTION WHEN duplicate_object THEN null;
END $$;

DO $$ BEGIN
    CREATE TRIGGER update_org_sso_configs_updated_at BEFORE UPDATE ON org_sso_configs FOR EACH ROW EXECUTE FUNCTION update_updated_at_column();
EXCEPTION WHEN duplicate_object THEN null;
END $$;

DO $$ BEGIN
    CREATE TRIGGER update_domain_verifications_updated_at BEFORE UPDATE ON domain_verifications FOR EACH ROW EXECUTE FUNCTION update_updated_at_column();
EXCEPTION WHEN duplicate_object THEN null;
END $$;

DO $$ BEGIN
    CREATE TRIGGER update_org_scim_configs_updated_at BEFORE UPDATE ON org_scim_configs FOR EACH ROW EXECUTE FUNCTION update_updated_at_column();
EXCEPTION WHEN duplicate_object THEN null;
END $$;

DO $$ BEGIN
    CREATE TRIGGER update_scim_user_mappings_updated_at BEFORE UPDATE ON scim_user_mappings FOR EACH ROW EXECUTE FUNCTION update_updated_at_column();
EXCEPTION WHEN duplicate_object THEN null;
END $$;

DO $$ BEGIN
    CREATE TRIGGER update_scim_group_mappings_updated_at BEFORE UPDATE ON scim_group_mappings FOR EACH ROW EXECUTE FUNCTION update_updated_at_column();
EXCEPTION WHEN duplicate_object THEN null;
END $$;

DO $$ BEGIN
    CREATE TRIGGER update_org_rbac_policies_updated_at BEFORE UPDATE ON org_rbac_policies FOR EACH ROW EXECUTE FUNCTION update_updated_at_column();
EXCEPTION WHEN duplicate_object THEN null;
END $$;

-- ======================================================================
-- Dead Letter Queue
-- ======================================================================

-- Stores failed operations (e.g., usage logging) for later recovery or inspection
CREATE TABLE IF NOT EXISTS dead_letter_queue (
    id UUID PRIMARY KEY NOT NULL,
    entry_type VARCHAR(64) NOT NULL,
    payload TEXT NOT NULL,
    error TEXT NOT NULL,
    -- Metadata (JSON)
    metadata JSONB NOT NULL DEFAULT '{}',
    retry_count INTEGER NOT NULL DEFAULT 0,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    last_retry_at TIMESTAMPTZ
);

CREATE INDEX IF NOT EXISTS idx_dlq_entry_type ON dead_letter_queue(entry_type);
CREATE INDEX IF NOT EXISTS idx_dlq_created_at ON dead_letter_queue(created_at);
CREATE INDEX IF NOT EXISTS idx_dlq_retry_count ON dead_letter_queue(retry_count);

-- ======================================================================
-- Conversations
-- ======================================================================

-- Chat message history storage.
-- pin_order: NULL = not pinned, 0-N = pinned with order (lower = higher in list)

DO $$ BEGIN
    CREATE TYPE conversation_owner_type AS ENUM ('project', 'user');
EXCEPTION
    WHEN duplicate_object THEN null;
END $$;

CREATE TABLE IF NOT EXISTS conversations (
    id UUID PRIMARY KEY NOT NULL,
    owner_type conversation_owner_type NOT NULL,
    owner_id UUID NOT NULL,
    title VARCHAR(255) NOT NULL,
    -- Model configuration (JSON array)
    models JSONB NOT NULL DEFAULT '[]',
    -- Message history (JSON array)
    messages JSONB NOT NULL DEFAULT '[]',
    pin_order INTEGER,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    deleted_at TIMESTAMPTZ
);

CREATE INDEX IF NOT EXISTS idx_conversations_owner ON conversations(owner_type, owner_id);
CREATE INDEX IF NOT EXISTS idx_conversations_created_at ON conversations(created_at);
-- Partial index for non-deleted conversations (most queries filter by deleted_at IS NULL)
CREATE INDEX IF NOT EXISTS idx_conversations_owner_active ON conversations(owner_type, owner_id) WHERE deleted_at IS NULL;
-- Index for pinned conversations (for efficient pinned queries per owner)
CREATE INDEX IF NOT EXISTS idx_conversations_owner_pinned ON conversations(owner_type, owner_id, pin_order) WHERE pin_order IS NOT NULL AND deleted_at IS NULL;

DO $$ BEGIN
    CREATE TRIGGER update_conversations_updated_at BEFORE UPDATE ON conversations FOR EACH ROW EXECUTE FUNCTION update_updated_at_column();
EXCEPTION WHEN duplicate_object THEN null;
END $$;

-- ======================================================================
-- Audit Logs
-- ======================================================================

-- Tracks admin operations for compliance and debugging

DO $$ BEGIN
    CREATE TYPE audit_actor_type AS ENUM ('user', 'api_key', 'system');
EXCEPTION
    WHEN duplicate_object THEN null;
END $$;

CREATE TABLE IF NOT EXISTS audit_logs (
    id UUID PRIMARY KEY NOT NULL,
    -- Who performed the action
    actor_type audit_actor_type NOT NULL,
    -- ID of the actor (user_id or api_key_id, NULL for system)
    actor_id UUID,
    -- The action performed (e.g., 'api_key.create', 'user.update')
    action VARCHAR(64) NOT NULL,
    -- Type of resource affected (e.g., 'api_key', 'user', 'organization')
    resource_type VARCHAR(64) NOT NULL,
    -- ID of the affected resource
    resource_id UUID NOT NULL,
    -- Optional organization context
    org_id UUID REFERENCES organizations(id) ON DELETE SET NULL,
    -- Optional project context
    project_id UUID REFERENCES projects(id) ON DELETE SET NULL,
    -- JSON with additional details (request info, before/after values, etc.)
    details JSONB NOT NULL DEFAULT '{}',
    -- Client IP address
    ip_address VARCHAR(45),
    -- Client user agent
    user_agent TEXT,
    -- When the action occurred
    timestamp TIMESTAMPTZ NOT NULL DEFAULT NOW()
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

-- Owner type for files and vector stores
DO $$ BEGIN
    CREATE TYPE vector_store_owner_type AS ENUM ('organization', 'team', 'project', 'user');
EXCEPTION
    WHEN duplicate_object THEN null;
END $$;

-- File purpose (OpenAI Files API compatible)
DO $$ BEGIN
    CREATE TYPE file_purpose AS ENUM ('assistants', 'batch', 'fine-tune', 'vision');
EXCEPTION
    WHEN duplicate_object THEN null;
END $$;

-- File status (OpenAI Files API compatible)
DO $$ BEGIN
    CREATE TYPE file_status AS ENUM ('uploaded', 'processed', 'error');
EXCEPTION
    WHEN duplicate_object THEN null;
END $$;

-- Storage backend type for files
DO $$ BEGIN
    CREATE TYPE file_storage_backend AS ENUM ('database', 'filesystem', 's3');
EXCEPTION
    WHEN duplicate_object THEN null;
END $$;

CREATE TABLE IF NOT EXISTS files (
    id UUID PRIMARY KEY NOT NULL,
    -- Ownership (who can access this file)
    owner_type vector_store_owner_type NOT NULL,
    owner_id UUID NOT NULL,
    -- File metadata
    filename VARCHAR(255) NOT NULL,
    purpose file_purpose NOT NULL DEFAULT 'assistants',
    content_type VARCHAR(128),
    size_bytes BIGINT NOT NULL,
    -- SHA-256 hash of file content for deduplication (64 hex characters)
    content_hash VARCHAR(64),
    -- Processing status
    status file_status NOT NULL DEFAULT 'uploaded',
    status_details TEXT,
    -- Storage
    storage_backend file_storage_backend NOT NULL DEFAULT 'database',
    file_data BYTEA,
    storage_path TEXT,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    expires_at TIMESTAMPTZ
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

-- Collection status (OpenAI VectorStore compatible)
DO $$ BEGIN
    CREATE TYPE collection_status AS ENUM ('in_progress', 'completed', 'expired');
EXCEPTION
    WHEN duplicate_object THEN null;
END $$;

-- File processing status (OpenAI VectorStoreFile compatible)
DO $$ BEGIN
    CREATE TYPE collection_file_status AS ENUM ('in_progress', 'completed', 'cancelled', 'failed');
EXCEPTION
    WHEN duplicate_object THEN null;
END $$;

CREATE TABLE IF NOT EXISTS vector_stores (
    id UUID PRIMARY KEY NOT NULL,
    -- Ownership (who can access this vector store)
    owner_type vector_store_owner_type NOT NULL,
    owner_id UUID NOT NULL,
    name VARCHAR(255) NOT NULL,
    description TEXT,
    -- Embedding configuration (set at creation, immutable)
    embedding_model VARCHAR(128) NOT NULL DEFAULT 'text-embedding-3-small',
    embedding_dimensions INTEGER NOT NULL DEFAULT 1536,
    status collection_status NOT NULL DEFAULT 'completed',
    -- Usage statistics
    usage_bytes BIGINT NOT NULL DEFAULT 0,
    -- File counts as JSON: {"cancelled":0, "completed":0, "failed":0, "in_progress":0, "total":0}
    file_counts JSONB NOT NULL DEFAULT '{"cancelled":0,"completed":0,"failed":0,"in_progress":0,"total":0}',
    -- Custom metadata (up to 16 key-value pairs, OpenAI-compatible)
    metadata JSONB,
    -- Expiration policy: {"anchor": "last_active_at", "days": N}
    expires_after JSONB,
    expires_at TIMESTAMPTZ,
    last_active_at TIMESTAMPTZ,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    deleted_at TIMESTAMPTZ,
    -- Unique name per owner
    UNIQUE(owner_type, owner_id, name)
);

CREATE INDEX IF NOT EXISTS idx_vector_stores_owner ON vector_stores(owner_type, owner_id);
-- Partial index for non-deleted vector_stores (most queries filter by deleted_at IS NULL)
CREATE INDEX IF NOT EXISTS idx_vector_stores_owner_active ON vector_stores(owner_type, owner_id) WHERE deleted_at IS NULL;
CREATE INDEX IF NOT EXISTS idx_vector_stores_status ON vector_stores(status);
CREATE INDEX IF NOT EXISTS idx_vector_stores_expires_at ON vector_stores(expires_at) WHERE expires_at IS NOT NULL;
CREATE INDEX IF NOT EXISTS idx_vector_stores_embedding_model ON vector_stores(embedding_model);

DO $$ BEGIN
    CREATE TRIGGER update_collections_updated_at BEFORE UPDATE ON vector_stores FOR EACH ROW EXECUTE FUNCTION update_updated_at_column();
EXCEPTION WHEN duplicate_object THEN null;
END $$;

-- ======================================================================
-- Vector Store Files
-- ======================================================================

-- Links files to vector stores. Follows OpenAI VectorStoreFile schema.
CREATE TABLE IF NOT EXISTS vector_store_files (
    id UUID PRIMARY KEY NOT NULL,
    vector_store_id UUID NOT NULL REFERENCES vector_stores(id) ON DELETE CASCADE,
    file_id UUID NOT NULL REFERENCES files(id),
    -- Processing status
    status collection_file_status NOT NULL DEFAULT 'in_progress',
    -- Processing statistics
    usage_bytes BIGINT NOT NULL DEFAULT 0,
    -- Error information (if status = failed): {"code": "string", "message": "string"}
    last_error JSONB,
    -- Chunking strategy: {"type": "auto"|"static", "static": {"max_chunk_size_tokens": N, "chunk_overlap_tokens": N}}
    chunking_strategy JSONB,
    -- Custom attributes for filtering (up to 16 key-value pairs)
    attributes JSONB,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    -- Soft delete timestamp (NULL = not deleted)
    deleted_at TIMESTAMPTZ
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

DO $$ BEGIN
    CREATE TRIGGER update_vector_store_files_updated_at BEFORE UPDATE ON vector_store_files FOR EACH ROW EXECUTE FUNCTION update_updated_at_column();
EXCEPTION WHEN duplicate_object THEN null;
END $$;

-- Note: Document chunks are stored in the vector database (pgvector or Qdrant),
-- not in the relational database. This enables efficient similarity search
-- without cross-database joins. See VectorStore trait for chunk operations.

-- ======================================================================
-- Templates
-- ======================================================================

-- Reusable system prompt templates.

DO $$ BEGIN
    CREATE TYPE template_owner_type AS ENUM ('organization', 'team', 'project', 'user');
EXCEPTION
    WHEN duplicate_object THEN null;
END $$;

CREATE TABLE IF NOT EXISTS templates (
    id UUID PRIMARY KEY NOT NULL,
    -- Ownership (who can access this template)
    owner_type template_owner_type NOT NULL,
    owner_id UUID NOT NULL,
    name VARCHAR(255) NOT NULL,
    description TEXT,
    -- The actual prompt content (system message template)
    content TEXT NOT NULL,
    -- Optional metadata (temperature, max_tokens, etc.)
    metadata JSONB,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    deleted_at TIMESTAMPTZ,
    -- Unique name per owner
    UNIQUE(owner_type, owner_id, name)
);

CREATE INDEX IF NOT EXISTS idx_templates_owner ON templates(owner_type, owner_id);
-- Partial index for non-deleted templates (most queries filter by deleted_at IS NULL)
CREATE INDEX IF NOT EXISTS idx_templates_owner_active ON templates(owner_type, owner_id) WHERE deleted_at IS NULL;
CREATE INDEX IF NOT EXISTS idx_templates_name ON templates(name);

DO $$ BEGIN
    CREATE TRIGGER update_templates_updated_at BEFORE UPDATE ON templates FOR EACH ROW EXECUTE FUNCTION update_updated_at_column();
EXCEPTION WHEN duplicate_object THEN null;
END $$;

-- ======================================================================
-- Service Accounts
-- ======================================================================

-- First-class machine identities that can own API keys and carry roles for
-- RBAC evaluation. Enables unified authorization across human users and
-- machine identities.
CREATE TABLE IF NOT EXISTS service_accounts (
    id UUID PRIMARY KEY NOT NULL,
    org_id UUID NOT NULL REFERENCES organizations(id) ON DELETE CASCADE,
    slug VARCHAR(64) NOT NULL,
    name VARCHAR(255) NOT NULL,
    description TEXT,
    -- JSON array of role strings (e.g., '["admin", "developer"]')
    -- These roles flow into the RBAC Subject when authenticating via API key
    roles JSONB NOT NULL DEFAULT '[]',
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    deleted_at TIMESTAMPTZ,
    UNIQUE(org_id, slug)
);

CREATE INDEX IF NOT EXISTS idx_service_accounts_org_id ON service_accounts(org_id);
CREATE INDEX IF NOT EXISTS idx_service_accounts_slug ON service_accounts(slug);
-- Partial indexes for non-deleted service accounts (most queries filter by deleted_at IS NULL)
CREATE INDEX IF NOT EXISTS idx_service_accounts_org_active ON service_accounts(org_id) WHERE deleted_at IS NULL;
CREATE INDEX IF NOT EXISTS idx_service_accounts_org_slug_active ON service_accounts(org_id, slug) WHERE deleted_at IS NULL;

DO $$ BEGIN
    CREATE TRIGGER update_service_accounts_updated_at BEFORE UPDATE ON service_accounts FOR EACH ROW EXECUTE FUNCTION update_updated_at_column();
EXCEPTION WHEN duplicate_object THEN null;
END $$;

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

DO $$ BEGIN
    CREATE TYPE skill_owner_type AS ENUM ('organization', 'team', 'project', 'user');
EXCEPTION
    WHEN duplicate_object THEN null;
END $$;

CREATE TABLE IF NOT EXISTS skills (
    id UUID PRIMARY KEY NOT NULL,
    owner_type skill_owner_type NOT NULL,
    owner_id UUID NOT NULL,
    -- Per spec: 1..=64 chars, [a-z0-9-]+, no leading/trailing/consecutive hyphens
    name VARCHAR(64) NOT NULL,
    -- Per spec: required, 1..=1024 chars
    description VARCHAR(1024) NOT NULL,
    -- Optional frontmatter fields (NULL = not set)
    user_invocable BOOLEAN,                     -- defaults to true in code
    disable_model_invocation BOOLEAN,           -- defaults to false in code
    allowed_tools JSONB,                        -- array of tool names
    argument_hint VARCHAR(255),
    source_url VARCHAR(2048),                   -- origin URL (e.g. GitHub) if imported
    source_ref VARCHAR(255),                    -- git ref if imported
    frontmatter_extra JSONB,                    -- unknown/forward-compat keys
    -- Cached sum of skill_files.byte_size for fast limit checks
    total_bytes BIGINT NOT NULL DEFAULT 0,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    deleted_at TIMESTAMPTZ,
    UNIQUE(owner_type, owner_id, name)
);

CREATE INDEX IF NOT EXISTS idx_skills_owner ON skills(owner_type, owner_id);
-- Partial index for non-deleted skills (most queries filter by deleted_at IS NULL)
CREATE INDEX IF NOT EXISTS idx_skills_owner_active ON skills(owner_type, owner_id) WHERE deleted_at IS NULL;
CREATE INDEX IF NOT EXISTS idx_skills_name ON skills(name);

DO $$ BEGIN
    CREATE TRIGGER update_skills_updated_at BEFORE UPDATE ON skills FOR EACH ROW EXECUTE FUNCTION update_updated_at_column();
EXCEPTION WHEN duplicate_object THEN null;
END $$;

-- Files bundled into a skill. Every skill must have exactly one row with
-- path = 'SKILL.md' (enforced in service layer). Additional rows hold
-- bundled scripts/references/assets referenced from SKILL.md.
CREATE TABLE IF NOT EXISTS skill_files (
    skill_id UUID NOT NULL REFERENCES skills(id) ON DELETE CASCADE,
    -- Relative path inside the skill directory (e.g. 'SKILL.md', 'scripts/extract.py')
    path VARCHAR(255) NOT NULL,
    content TEXT NOT NULL,
    -- Cached byte length of content for fast total-size aggregation
    byte_size BIGINT NOT NULL,
    -- MIME type; defaults to 'text/markdown' for SKILL.md, sniffed from
    -- extension for others
    content_type VARCHAR(127) NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    PRIMARY KEY(skill_id, path)
);

CREATE INDEX IF NOT EXISTS idx_skill_files_skill ON skill_files(skill_id);

DO $$ BEGIN
    CREATE TRIGGER update_skill_files_updated_at BEFORE UPDATE ON skill_files FOR EACH ROW EXECUTE FUNCTION update_updated_at_column();
EXCEPTION WHEN duplicate_object THEN null;
END $$;

-- ======================================================================
-- OAuth PKCE Authorization Codes
-- ======================================================================

-- Short-lived, single-use codes issued when a user grants consent on the
-- /oauth/authorize page. The external app exchanges the code (plus its
-- code_verifier) at /oauth/token to receive a user-scoped API key.
--
-- Codes are bound to a single user, callback URL, and PKCE challenge. The
-- `used_at` column is set atomically on exchange to prevent replay.

DO $$ BEGIN
    CREATE TYPE oauth_pkce_method AS ENUM ('S256', 'plain');
EXCEPTION
    WHEN duplicate_object THEN null;
END $$;

CREATE TABLE IF NOT EXISTS oauth_authorization_codes (
    id UUID PRIMARY KEY NOT NULL,
    -- Random opaque code returned to the external app via the callback URL
    code VARCHAR(128) NOT NULL UNIQUE,
    -- PKCE challenge supplied by the external app
    code_challenge VARCHAR(255) NOT NULL,
    code_challenge_method oauth_pkce_method NOT NULL,
    -- Where the user is sent after granting consent; the external app must use
    -- the exact same callback URL when redeeming the code
    callback_url VARCHAR(2048) NOT NULL,
    -- The user who granted consent
    user_id UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    -- Optional human-readable identifier displayed on the consent screen
    app_name VARCHAR(255),
    -- The user's choices on the consent page (label, budget, scopes, model
    -- restrictions, etc.) — applied to the issued key on exchange. Stored
    -- as JSONB so we can extend the option set without a migration.
    key_options JSONB NOT NULL DEFAULT '{}'::jsonb,
    expires_at TIMESTAMPTZ NOT NULL,
    used_at TIMESTAMPTZ,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE INDEX IF NOT EXISTS idx_oauth_authz_codes_code ON oauth_authorization_codes(code);
CREATE INDEX IF NOT EXISTS idx_oauth_authz_codes_user ON oauth_authorization_codes(user_id);
-- Used by the periodic cleanup query to find expired/consumed codes
CREATE INDEX IF NOT EXISTS idx_oauth_authz_codes_expires ON oauth_authorization_codes(expires_at);

-- ======================================================================
-- Responses (Responses API persistence)
-- ======================================================================

DO $$ BEGIN
    CREATE TYPE response_owner_type AS ENUM ('organization', 'team', 'project', 'user', 'service_account');
EXCEPTION
    WHEN duplicate_object THEN null;
END $$;

-- See SQLite migration for documentation. Mirror schema.
--
-- `owner_type`/`owner_id` follow the same pattern as `skills` /
-- `templates` / `conversations`: they record which scope a response
-- belongs to (so it can be listed/retrieved as an org/team/project/
-- user/service-account resource). `org_id` is the tenant boundary;
-- the audit columns (`user_id`, `api_key_id`, `project_id`,
-- `service_account_id`) record who actually made the call.
CREATE TABLE IF NOT EXISTS responses (
    id VARCHAR(64) PRIMARY KEY,
    org_id UUID NOT NULL REFERENCES organizations(id) ON DELETE CASCADE,
    owner_type response_owner_type NOT NULL,
    owner_id UUID NOT NULL,
    project_id UUID REFERENCES projects(id) ON DELETE SET NULL,
    user_id UUID REFERENCES users(id) ON DELETE SET NULL,
    api_key_id UUID REFERENCES api_keys(id) ON DELETE SET NULL,
    service_account_id UUID REFERENCES service_accounts(id) ON DELETE SET NULL,
    status VARCHAR(16) NOT NULL,
    background BOOLEAN NOT NULL DEFAULT FALSE,
    model VARCHAR(128) NOT NULL,
    provider VARCHAR(128),
    created_at TIMESTAMPTZ NOT NULL,
    started_at TIMESTAMPTZ,
    completed_at TIMESTAMPTZ,
    request_payload JSONB NOT NULL,
    output JSONB,
    usage JSONB,
    error JSONB,
    retention_expires_at TIMESTAMPTZ NOT NULL,
    last_sequence_number BIGINT NOT NULL DEFAULT 0
);

CREATE INDEX IF NOT EXISTS idx_responses_org_status ON responses(org_id, status);
CREATE INDEX IF NOT EXISTS idx_responses_owner_created ON responses(owner_type, owner_id, created_at DESC);
CREATE INDEX IF NOT EXISTS idx_responses_retention ON responses(retention_expires_at);

-- Append-only event log; see SQLite migration. The composite PRIMARY
-- KEY already provides the (response_id, sequence_number) b-tree, so
-- no extra index is needed.
CREATE TABLE IF NOT EXISTS response_events (
    response_id VARCHAR(64) NOT NULL REFERENCES responses(id) ON DELETE CASCADE,
    sequence_number BIGINT NOT NULL,
    event_type VARCHAR(64) NOT NULL,
    payload JSONB NOT NULL,
    created_at TIMESTAMPTZ NOT NULL,
    PRIMARY KEY (response_id, sequence_number)
);
