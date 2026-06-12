import { zodResolver } from "@hookform/resolvers/zod";
import { useState } from "react";
import { useForm } from "react-hook-form";
import { Navigate, useLocation } from "react-router-dom";
import { z } from "zod";

import { useAuth, useDiscoverSso, type DiscoveryResult } from "@/auth";
import { Button } from "@/components/Button/Button";
import { Card, CardContent, CardHeader, CardTitle } from "@/components/Card/Card";
import { FormField } from "@/components/FormField/FormField";
import { HadrianIcon } from "@/components/HadrianIcon/HadrianIcon";
import { Input } from "@/components/Input/Input";
import { useConfig } from "@/config/ConfigProvider";
import { usePreferences } from "@/preferences/PreferencesProvider";

const loginSchema = z.object({
  apiKey: z.string().min(1, "API key is required"),
});

const emailSchema = z.object({
  email: z.string().email("Invalid email address"),
});

type LoginForm = z.infer<typeof loginSchema>;
type EmailForm = z.infer<typeof emailSchema>;

export default function LoginPage() {
  const { config, isLoading: configLoading } = useConfig();
  const { isAuthenticated, isLoading: authLoading, login } = useAuth();
  const { resolvedTheme } = usePreferences();
  const location = useLocation();
  const discoverSso = useDiscoverSso();

  const [error, setError] = useState<string | null>(null);
  const [discoveredOrg, setDiscoveredOrg] = useState<DiscoveryResult | null>(null);
  const [discoveryEmail, setDiscoveryEmail] = useState<string>("");

  // Get login-specific config with fallbacks
  const loginConfig = config?.branding.login;
  const showLogo = loginConfig?.show_logo ?? true;
  const loginTitle = loginConfig?.title || config?.branding.title || "Hadrian Gateway";
  const loginSubtitle = loginConfig?.subtitle || config?.branding.tagline || "Sign in to continue";
  const backgroundImage = loginConfig?.background_image;

  // Determine which logo to use based on theme
  const logoUrl =
    resolvedTheme === "dark" && config?.branding.logo_dark_url
      ? config.branding.logo_dark_url
      : config?.branding.logo_url;

  const apiKeyForm = useForm<LoginForm>({
    resolver: zodResolver(loginSchema),
    defaultValues: { apiKey: "" },
  });

  const emailForm = useForm<EmailForm>({
    resolver: zodResolver(emailSchema),
    defaultValues: { email: "" },
  });

  // Prefer an explicit `?return_to=` query param so flows that need to preserve
  // a full URL (path + search, e.g. /oauth/authorize?callback_url=...) survive
  // the round-trip through login. Falls back to the in-app `state.from` set by
  // RequireAuth.
  //
  // `startsWith("/")` alone is not enough: `//evil.com/...` and `/\evil.com`
  // are treated as same-origin by `Navigate`/`startsWith` but resolve to a
  // cross-origin URL in the browser. Reject anything whose second character
  // makes it protocol-relative or backslash-prefixed.
  const isSafeReturnTo = (value: string | null): value is string =>
    !!value && value.startsWith("/") && !value.startsWith("//") && !value.startsWith("/\\");
  const returnToParam = new URLSearchParams(location.search).get("return_to");
  const from = isSafeReturnTo(returnToParam)
    ? returnToParam
    : location.state?.from?.pathname || "/";

  if (configLoading || authLoading) {
    return (
      <div className="flex min-h-screen items-center justify-center bg-background">
        <div className="text-muted-foreground">Loading...</div>
      </div>
    );
  }

  if (isAuthenticated) {
    return <Navigate to={from} replace />;
  }

  const authMethods = config?.auth.methods || ["api_key"];
  const hasApiKey = authMethods.includes("api_key");
  const hasOidc = authMethods.includes("oidc") && config?.auth.oidc;
  const hasPerOrgSso = authMethods.includes("per_org_sso");
  const hasEmailDiscovery = hasOidc || hasPerOrgSso;
  // "session" alone (IdP mode before any org SSO config is enabled) means the
  // login page has no flow that can succeed — /auth/discover has nothing to
  // find — so show setup guidance instead of a discovery form that always
  // dead-ends.
  const hasSession = authMethods.includes("session");
  const ssoNotConfigured = hasSession && !hasEmailDiscovery && !hasApiKey;

  const onApiKeySubmit = async (data: LoginForm) => {
    setError(null);
    try {
      await login("api_key", { apiKey: data.apiKey });
    } catch (err) {
      setError(err instanceof Error ? err.message : "Authentication failed");
    }
  };

  const isSubmitting = apiKeyForm.formState.isSubmitting;

  const handleOidcLogin = (orgId?: string) => {
    login("oidc", orgId ? { orgId } : undefined);
  };

  const handleSsoLogin = (org: DiscoveryResult) => {
    // Build the appropriate login URL based on provider type
    const returnTo = encodeURIComponent(from);
    const orgParam = encodeURIComponent(org.org_id);

    if (org.provider_type === "saml") {
      // SAML uses a separate login endpoint
      window.location.href = `/auth/saml/login?org=${orgParam}&return_to=${returnTo}`;
    } else {
      // OIDC uses the standard login endpoint (which auto-dispatches based on org config)
      login("oidc", { orgId: org.org_id });
    }
  };

  const handleEmailDiscovery = async (data: EmailForm) => {
    setError(null);
    setDiscoveryEmail(data.email);

    discoverSso.mutate(data.email, {
      onSuccess: (result) => {
        setDiscoveredOrg(result);
        // If SSO is required, auto-redirect to the appropriate login endpoint
        if (result.sso_required && result.has_sso) {
          handleSsoLogin(result);
        }
      },
      onError: (err) => {
        // If discovery fails, user might not have org SSO configured
        // Just clear discovery state and let them use other methods
        setDiscoveredOrg(null);
        // Only show error if they typed an email (not on initial load)
        if (data.email) {
          setError(err.message || "No SSO configuration found for this email domain");
        }
      },
    });
  };

  const handleClearDiscovery = () => {
    setDiscoveredOrg(null);
    setDiscoveryEmail("");
    emailForm.reset();
    setError(null);
  };

  return (
    <div
      className="flex min-h-screen items-center justify-center bg-background bg-cover bg-center p-4"
      style={backgroundImage ? { backgroundImage: `url(${backgroundImage})` } : undefined}
    >
      <Card className="w-full max-w-md">
        <CardHeader className="text-center">
          {showLogo && (
            <div className="mx-auto mb-4">
              {logoUrl ? (
                <img src={logoUrl} alt={loginTitle} className="h-16 w-16 object-contain mx-auto" />
              ) : (
                <div className="flex h-16 w-16 items-center justify-center rounded-xl bg-primary/10 mx-auto">
                  <HadrianIcon size={32} className="text-primary" />
                </div>
              )}
            </div>
          )}
          <CardTitle className="text-2xl">{loginTitle}</CardTitle>
          <p className="text-muted-foreground">{loginSubtitle}</p>
        </CardHeader>
        <CardContent className="space-y-6">
          {error && (
            <div className="rounded-md bg-destructive/10 p-3 text-sm text-destructive">{error}</div>
          )}

          {/* Email discovery step for per-org SSO */}
          {hasEmailDiscovery && !discoveredOrg && (
            <form onSubmit={emailForm.handleSubmit(handleEmailDiscovery)} className="space-y-4">
              <FormField
                label="Work Email"
                htmlFor="email"
                helpText="Enter your work email to find your organization's sign-in method"
                error={emailForm.formState.errors.email?.message}
              >
                <Input
                  id="email"
                  type="email"
                  placeholder="you@company.com"
                  {...emailForm.register("email")}
                  disabled={discoverSso.isPending}
                />
              </FormField>
              <Button
                type="submit"
                className="w-full"
                disabled={discoverSso.isPending || !emailForm.watch("email")}
              >
                {discoverSso.isPending ? "Looking up..." : "Continue"}
              </Button>
            </form>
          )}

          {/* Show discovered org SSO button */}
          {hasEmailDiscovery && discoveredOrg && discoveredOrg.has_sso && (
            <div className="space-y-4">
              <div className="rounded-md bg-muted p-3 text-sm">
                <div className="font-medium">{discoveredOrg.org_name}</div>
                <div className="text-muted-foreground">Signing in as {discoveryEmail}</div>
              </div>
              {/* Test mode banner */}
              {discoveredOrg.enforcement_mode === "test" && (
                <div className="rounded-md bg-blue-50 dark:bg-blue-900/20 p-3 text-sm text-blue-800 dark:text-blue-200 border border-blue-200 dark:border-blue-800">
                  <div className="font-medium">SSO enforcement testing</div>
                  <div className="text-blue-700 dark:text-blue-300 mt-1">
                    Your organization is testing SSO enforcement. You can still use other sign-in
                    methods, but SSO will soon be required.
                  </div>
                </div>
              )}
              <Button
                className="w-full"
                onClick={() => handleSsoLogin(discoveredOrg)}
                disabled={isSubmitting}
              >
                Sign in with {discoveredOrg.idp_name || "SSO"}
              </Button>
              {!discoveredOrg.sso_required && (
                <button
                  type="button"
                  onClick={handleClearDiscovery}
                  className="w-full text-sm text-muted-foreground hover:text-foreground"
                >
                  Use a different sign-in method
                </button>
              )}
            </div>
          )}

          {/* Domain verification pending/failed - show info message */}
          {hasEmailDiscovery &&
            discoveredOrg &&
            !discoveredOrg.has_sso &&
            !discoveredOrg.domain_verified && (
              <div className="space-y-4">
                <div className="rounded-md bg-muted p-3 text-sm">
                  <div className="font-medium">{discoveredOrg.org_name}</div>
                  <div className="text-muted-foreground">
                    Found SSO configuration for {discoveryEmail}
                  </div>
                </div>
                <div className="rounded-md bg-warning/10 p-3 text-sm text-warning-foreground border border-warning/20">
                  {discoveredOrg.domain_verification_status === "pending" ? (
                    <>
                      <div className="font-medium">Domain verification pending</div>
                      <div className="text-muted-foreground mt-1">
                        Your organization&apos;s SSO domain is awaiting DNS verification. Contact
                        your IT administrator to complete the setup.
                      </div>
                    </>
                  ) : discoveredOrg.domain_verification_status === "failed" ? (
                    <>
                      <div className="font-medium">Domain verification failed</div>
                      <div className="text-muted-foreground mt-1">
                        Your organization&apos;s SSO domain could not be verified. Contact your IT
                        administrator to resolve this issue.
                      </div>
                    </>
                  ) : (
                    <>
                      <div className="font-medium">Domain verification required</div>
                      <div className="text-muted-foreground mt-1">
                        SSO is configured for your organization, but the email domain has not been
                        verified. Contact your IT administrator to set up domain verification.
                      </div>
                    </>
                  )}
                </div>
                <button
                  type="button"
                  onClick={handleClearDiscovery}
                  className="w-full text-sm text-muted-foreground hover:text-foreground"
                >
                  Use a different sign-in method
                </button>
              </div>
            )}

          {/* Global OIDC fallback (when no org SSO discovered but email entered) */}
          {hasOidc && discoveredOrg === null && discoveryEmail && (
            <div>
              <Button className="w-full" onClick={() => handleOidcLogin()} disabled={isSubmitting}>
                Sign in with {config?.auth.oidc?.provider || "SSO"}
              </Button>
            </div>
          )}

          {/* Per-org SSO not found message (when only per-org SSO is available, no global OIDC) */}
          {hasPerOrgSso && !hasOidc && discoveredOrg === null && discoveryEmail && (
            <div className="rounded-md bg-muted p-3 text-sm text-muted-foreground">
              No SSO configuration found for this email domain.
              <button
                type="button"
                onClick={handleClearDiscovery}
                className="block mt-2 text-primary hover:underline"
              >
                Try a different email
              </button>
            </div>
          )}

          {/* Divider between SSO and API key */}
          {hasEmailDiscovery && hasApiKey && !discoveredOrg?.sso_required && (
            <div className="relative">
              <div className="absolute inset-0 flex items-center">
                <span className="w-full border-t" />
              </div>
              <div className="relative flex justify-center text-xs uppercase">
                <span className="bg-card px-2 text-muted-foreground">Or continue with</span>
              </div>
            </div>
          )}

          {/* API Key form - hidden if SSO is required for discovered org */}
          {hasApiKey && !discoveredOrg?.sso_required && (
            <form onSubmit={apiKeyForm.handleSubmit(onApiKeySubmit)} className="space-y-4">
              <FormField
                label="API Key"
                htmlFor="api-key"
                error={apiKeyForm.formState.errors.apiKey?.message}
              >
                <Input
                  id="api-key"
                  type="password"
                  placeholder="gw_live_..."
                  {...apiKeyForm.register("apiKey")}
                  disabled={isSubmitting}
                />
              </FormField>
              <Button
                type="submit"
                variant="secondary"
                className="w-full"
                disabled={isSubmitting || !apiKeyForm.watch("apiKey")}
              >
                {isSubmitting ? "Signing in..." : "Sign in with API Key"}
              </Button>
            </form>
          )}

          {ssoNotConfigured && (
            <p className="text-center text-muted-foreground">
              Single sign-on is enabled, but no identity provider has been configured yet. Contact
              your administrator to complete the setup.
            </p>
          )}

          {!hasApiKey && !hasEmailDiscovery && !ssoNotConfigured && (
            <p className="text-center text-muted-foreground">
              No authentication methods available. Please check your configuration.
            </p>
          )}
        </CardContent>
      </Card>
    </div>
  );
}
