import { createContext, useCallback, useContext, useEffect, useMemo, useState } from "react";

import { useConfig } from "@/config/ConfigProvider";
import { useLocalStorage } from "@/hooks/useLocalStorage";

import { COOKIE_SESSION_METHODS, hasCookieSessionMethod } from "./types";
import type { AuthContextValue, AuthMethod, AuthState, LoginCredentials, User } from "./types";

export const AuthContext = createContext<AuthContextValue | null>(null);

const STORAGE_KEY = "hadrian-auth";

/**
 * TTL for the API key kept in `localStorage`. The proper fix is to move the
 * token into a httpOnly+Secure cookie, but that requires a backend session
 * the gateway doesn't currently issue for API-key logins. Until then, we cap
 * the on-disk lifetime so an exfiltrated localStorage entry stops being
 * useful within a day. Re-login refreshes the timestamp.
 */
const API_KEY_TTL_MS = 24 * 60 * 60 * 1000; // 24 hours

interface StoredAuth {
  method: AuthMethod;
  token: string;
  user?: User;
  /** Wall-clock expiry; absent on entries written by older builds (treated as
   *  expired so they get cleared on next load). */
  expiresAt?: number;
}

interface MeResponse {
  external_id: string;
  email?: string;
  name?: string;
  user_id?: string;
  roles?: string[];
}

/** Fetch current user identity from the server */
async function fetchMe(token?: string): Promise<User | null> {
  try {
    const headers: Record<string, string> = {};
    if (token) {
      headers.Authorization = `Bearer ${token}`;
    }
    const response = await fetch("/auth/me", {
      headers,
      credentials: "include",
    });
    if (response.ok) {
      const data: MeResponse = await response.json();
      return {
        id: data.user_id || data.external_id,
        email: data.email,
        name: data.name,
        roles: data.roles ?? [],
      };
    }
  } catch {
    // Failed to fetch user info - endpoint may not exist
  }
  return null;
}

export function AuthProvider({ children }: { children: React.ReactNode }) {
  const { config, isLoading: configLoading } = useConfig();
  const [storedAuth, setStoredAuth] = useLocalStorage<StoredAuth | null>(STORAGE_KEY, null);
  const [state, setState] = useState<AuthState>({
    isAuthenticated: false,
    isLoading: true,
    user: null,
    method: null,
    token: null,
  });

  // Check for header-based auth (zero-trust proxy). Probe `/auth/me` rather
  // than an admin endpoint so non-admin header-authenticated users (who cannot
  // list organizations) still resolve to an authenticated session.
  const checkHeaderAuth = useCallback(async (): Promise<{
    user: User;
    token: string;
  } | null> => {
    if (!config?.auth.methods.includes("header")) {
      return null;
    }

    const user = await fetchMe();
    return user ? { user, token: "header-auth" } : null;
  }, [config?.auth.methods]);

  // Initialize auth state
  useEffect(() => {
    if (configLoading) return;

    const initAuth = async () => {
      // Check if auth is disabled (none method)
      if (config?.auth.methods.includes("none")) {
        // Get user info from /auth/me - backend provides a default anonymous user
        const user = await fetchMe();
        setState({
          isAuthenticated: true,
          isLoading: false,
          user,
          method: "none",
          token: null,
        });
        return;
      }

      // First, check for header-based auth (zero-trust proxy)
      const headerAuth = await checkHeaderAuth();
      if (headerAuth) {
        setState({
          isAuthenticated: true,
          isLoading: false,
          user: headerAuth.user,
          method: "header",
          token: headerAuth.token,
        });
        return;
      }

      // Check for stored credentials. API-key entries written before the TTL
      // landed (or that have aged out) are evicted here so a long-stale token
      // doesn't keep authenticating the SPA forever.
      if (storedAuth) {
        const expired =
          storedAuth.method === "api_key" &&
          (storedAuth.expiresAt === undefined || storedAuth.expiresAt < Date.now());
        if (expired) {
          setStoredAuth(null);
          setState({
            isAuthenticated: false,
            isLoading: false,
            user: null,
            method: null,
            token: null,
          });
          return;
        }

        // Refresh user info from server (user_id may have changed)
        const user = await fetchMe(storedAuth.token);
        setState({
          isAuthenticated: true,
          isLoading: false,
          user: user || storedAuth.user || null,
          method: storedAuth.method,
          token: storedAuth.token,
        });
        // Update stored auth with fresh user info
        if (user && (!storedAuth.user || storedAuth.user.id !== user.id)) {
          setStoredAuth({ ...storedAuth, user });
        }
        return;
      }

      // Check for a cookie session by calling /auth/me. Every SSO flow
      // (global OIDC, IdP-mode per-org OIDC, SAML) ends with /auth/callback
      // or /auth/saml/acs setting an httpOnly cookie the SPA cannot read, so
      // the probe must run whenever ANY cookie-session method is advertised
      // (IdP mode advertises ["session", "per_org_sso"], never "oidc").
      if (hasCookieSessionMethod(config?.auth.methods)) {
        const user = await fetchMe();
        if (user) {
          setState({
            isAuthenticated: true,
            isLoading: false,
            user,
            method: "session",
            token: null, // Credential is the httpOnly cookie
          });
          return;
        }
      }

      // No authentication found
      setState({
        isAuthenticated: false,
        isLoading: false,
        user: null,
        method: null,
        token: null,
      });
    };

    initAuth();
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [configLoading, config?.auth.methods]);

  const login = useCallback(
    async (method: AuthMethod, credentials?: LoginCredentials): Promise<void> => {
      setState((prev) => ({ ...prev, isLoading: true }));

      try {
        if (method === "api_key" && credentials?.apiKey) {
          // Validate API key by making a test request
          const response = await fetch("/admin/v1/organizations?limit=1", {
            headers: {
              Authorization: `Bearer ${credentials.apiKey}`,
            },
          });

          if (!response.ok) {
            throw new Error("Invalid API key");
          }

          // Fetch user info
          const user = await fetchMe(credentials.apiKey);

          const authData: StoredAuth = {
            method: "api_key",
            token: credentials.apiKey,
            user: user || undefined,
            expiresAt: Date.now() + API_KEY_TTL_MS,
          };

          setStoredAuth(authData);
          setState({
            isAuthenticated: true,
            isLoading: false,
            user,
            method: "api_key",
            token: credentials.apiKey,
          });
        } else if (method === "oidc") {
          // Redirect to backend's OIDC login endpoint
          // The backend handles PKCE and state management
          // If orgId is provided, use per-organization SSO
          const url = credentials?.orgId
            ? `/auth/login?org=${encodeURIComponent(credentials.orgId)}`
            : "/auth/login";
          window.location.href = url;
        } else {
          throw new Error("Invalid auth method or missing credentials");
        }
      } catch (error) {
        setState({
          isAuthenticated: false,
          isLoading: false,
          user: null,
          method: null,
          token: null,
        });
        throw error;
      }
    },
    [setStoredAuth]
  );

  const logout = useCallback(() => {
    const hadCookieSession = state.method !== null && COOKIE_SESSION_METHODS.includes(state.method);
    setStoredAuth(null);
    setState({
      isAuthenticated: false,
      isLoading: false,
      user: null,
      method: null,
      token: null,
    });

    // Cookie sessions must be revoked server-side. The backend mounts
    // /auth/logout as POST only (CSRF-safe), so a plain navigation would
    // 405 and leave the session alive — POST first, then hard-navigate to
    // /login to drop user-scoped in-memory caches. If the request fails the
    // navigation still happens and the login page's session probe reports
    // the truth (a surviving cookie re-authenticates).
    if (hadCookieSession) {
      void fetch("/auth/logout", { method: "POST", credentials: "include" }).finally(() => {
        window.location.href = "/login";
      });
    }
  }, [setStoredAuth, state.method]);

  const setApiKey = useCallback(
    (apiKey: string) => {
      const authData: StoredAuth = {
        method: "api_key",
        token: apiKey,
      };
      setStoredAuth(authData);
      setState({
        isAuthenticated: true,
        isLoading: false,
        user: null,
        method: "api_key",
        token: apiKey,
      });
    },
    [setStoredAuth]
  );

  const value = useMemo<AuthContextValue>(
    () => ({
      ...state,
      login,
      logout,
      setApiKey,
    }),
    [state, login, logout, setApiKey]
  );

  return <AuthContext.Provider value={value}>{children}</AuthContext.Provider>;
}

export function useAuth(): AuthContextValue {
  const context = useContext(AuthContext);
  if (!context) {
    throw new Error("useAuth must be used within an AuthProvider");
  }
  return context;
}
