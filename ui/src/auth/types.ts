import type { OidcConfig as UiOidcConfig } from "@/config/types";

/**
 * Auth method identifiers. These name three related but distinct things:
 *
 * - Capabilities the backend advertises in `config.auth.methods` ("none",
 *   "api_key", "header", "session", "per_org_sso", legacy "oidc").
 * - The login flow passed to `login()` ("api_key", "oidc").
 * - The authenticated state of the SPA in `AuthState.method` ("none",
 *   "api_key", "header", "session") — how the *current* credential works,
 *   not the flow that produced it. Any SSO flow (global OIDC, per-org OIDC,
 *   SAML) ends in a cookie session, so all of them resolve to "session".
 */
export type AuthMethod = "none" | "api_key" | "oidc" | "header" | "session" | "per_org_sso";

/**
 * Advertised methods whose credential is an httpOnly session cookie set by
 * the backend (`/auth/callback` or `/auth/saml/acs`). The SPA cannot read the
 * cookie, so the only way to detect such a session is to probe `/auth/me`
 * with `credentials: "include"`. The gateway advertises "session" in IdP
 * mode and "per_org_sso" when org SSO configs exist; "oidc" is the legacy
 * global-OIDC value kept for compatibility.
 */
export const COOKIE_SESSION_METHODS: readonly AuthMethod[] = ["oidc", "session", "per_org_sso"];

/** Whether the advertised method set implies the user may hold a cookie session. */
export function hasCookieSessionMethod(methods: readonly string[] | undefined): boolean {
  return COOKIE_SESSION_METHODS.some((method) => methods?.includes(method));
}

export interface User {
  id: string;
  email?: string;
  name?: string;
  roles?: string[];
}

/** Admin roles that grant access to the admin UI */
export const ADMIN_ROLES = ["super_admin", "org_admin", "team_admin"] as const;

/** Check if a user has any admin role.
 *
 * The earlier shortcut "always allow in `import.meta.env.DEV`" leaked into
 * Storybook builds and any local production-ish setup with `pnpm dev`, so
 * the admin UI rendered for unprivileged users. Bypassing the role check now
 * requires an explicit opt-in via `VITE_FORCE_ADMIN_ACCESS=1` so each
 * developer turning it on is doing so deliberately. */
export function hasAdminAccess(user: User | null): boolean {
  if (import.meta.env.VITE_FORCE_ADMIN_ACCESS === "1") return true;

  if (!user?.roles) return false;
  return user.roles.some((role) => ADMIN_ROLES.includes(role as (typeof ADMIN_ROLES)[number]));
}

export interface AuthState {
  isAuthenticated: boolean;
  isLoading: boolean;
  user: User | null;
  method: AuthMethod | null;
  token: string | null;
}

// Re-export from config for convenience
export type OidcConfig = UiOidcConfig;

export interface AuthContextValue extends AuthState {
  login: (method: AuthMethod, credentials?: LoginCredentials) => Promise<void>;
  logout: () => void;
  setApiKey: (apiKey: string) => void;
}

export interface LoginCredentials {
  apiKey?: string;
  orgId?: string;
}

/** Domain verification status */
export type DomainVerificationStatus = "pending" | "verified" | "failed";

/** SSO enforcement mode */
export type SsoEnforcementMode = "optional" | "required" | "test";

/** SSO provider type */
export type SsoProviderType = "oidc" | "saml";

/** Response from the /auth/discover endpoint */
export interface DiscoveryResult {
  org_id: string;
  org_slug: string;
  org_name: string;
  /** Whether SSO is configured and the domain is verified. SSO is only available when both conditions are met. */
  has_sso: boolean;
  /** Whether SSO is required (only true if has_sso is also true). */
  sso_required: boolean;
  /**
   * The SSO enforcement mode for this organization.
   * - "optional": SSO is available but not required
   * - "required": SSO is required; non-SSO auth will be blocked
   * - "test": SSO enforcement is being tested; non-SSO auth is logged but allowed
   */
  enforcement_mode: SsoEnforcementMode;
  /**
   * The SSO provider type - determines which auth flow to use.
   * - "oidc": Use OpenID Connect flow (/auth/login)
   * - "saml": Use SAML 2.0 flow (/auth/saml/login)
   */
  provider_type: SsoProviderType;
  idp_name: string | null;
  /** Whether the email domain has been verified via DNS TXT record. */
  domain_verified: boolean;
  /** Current verification status of the domain (pending, verified, failed). */
  domain_verification_status?: DomainVerificationStatus;
  /** When the domain was successfully verified (ISO 8601 date string). */
  verified_at?: string;
}
