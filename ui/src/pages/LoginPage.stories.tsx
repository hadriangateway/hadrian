import type { Meta, StoryObj } from "@storybook/react";
import { expect, waitFor, within } from "storybook/test";
import { http, HttpResponse } from "msw";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { MemoryRouter } from "react-router-dom";
import LoginPage from "./LoginPage";
import { ConfigProvider } from "@/config/ConfigProvider";
import { AuthProvider } from "@/auth";
import type { UiConfig } from "@/config/types";

// Base config with API key auth (most common scenario)
const mockApiKeyConfig: UiConfig = {
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
    max_file_size_bytes: 10 * 1024 * 1024,
    allowed_file_types: [],
  },
  admin: { enabled: true },
  auth: {
    methods: ["api_key"],
    oidc: null,
  },
};

// Config with OIDC + API key auth
const mockOidcConfig: UiConfig = {
  ...mockApiKeyConfig,
  auth: {
    methods: ["oidc", "api_key"],
    oidc: {
      provider: "Okta",
      authorization_url: "https://login.example.com/oauth2/authorize",
      client_id: "hadrian-gateway",
    },
  },
};

// Config with per-org SSO + API key auth
const mockPerOrgSsoConfig: UiConfig = {
  ...mockApiKeyConfig,
  auth: {
    methods: ["per_org_sso", "api_key"],
    oidc: null,
  },
};

// IdP mode: cookie sessions + per-org SSO discovery, no API key fallback
const mockIdpConfig: UiConfig = {
  ...mockApiKeyConfig,
  auth: {
    methods: ["session", "per_org_sso"],
    oidc: null,
  },
};

// IdP mode before any org SSO config is enabled (bootstrap state)
const mockIdpNoOrgSsoConfig: UiConfig = {
  ...mockApiKeyConfig,
  auth: {
    methods: ["session"],
    oidc: null,
  },
};

// Config with custom branding
const mockBrandedConfig: UiConfig = {
  ...mockApiKeyConfig,
  branding: {
    ...mockApiKeyConfig.branding,
    title: "Acme Corp AI",
    tagline: "Enterprise AI Platform",
    login: {
      title: "Welcome to Acme AI",
      subtitle: "Sign in with your API key to get started",
      show_logo: true,
    },
  },
  auth: {
    methods: ["oidc", "api_key"],
    oidc: {
      provider: "Acme SSO",
      authorization_url: "https://sso.acme-corp.com/oauth2/authorize",
      client_id: "acme-ai-gateway",
    },
  },
};

// Config with no auth methods
const mockNoAuthConfig: UiConfig = {
  ...mockApiKeyConfig,
  auth: {
    methods: [],
    oidc: null,
  },
};

// Handlers that ensure the user is NOT authenticated (returns 401 for auth checks)
const unauthenticatedHandlers = [
  http.get("*/auth/me", () => {
    return new HttpResponse(null, { status: 401 });
  }),
  http.get("*/admin/v1/organizations", () => {
    return new HttpResponse(null, { status: 401 });
  }),
];

function createHandlers(config: UiConfig) {
  return [
    http.get("*/admin/v1/ui/config", () => {
      return HttpResponse.json(config);
    }),
    ...unauthenticatedHandlers,
  ];
}

const meta: Meta<typeof LoginPage> = {
  title: "Pages/LoginPage",
  component: LoginPage,
  decorators: [
    (Story) => {
      // Clear stored auth so the login page renders instead of redirecting
      localStorage.removeItem("hadrian-auth");

      const queryClient = new QueryClient({
        defaultOptions: { queries: { retry: false } },
      });
      return (
        <QueryClientProvider client={queryClient}>
          <MemoryRouter>
            <ConfigProvider>
              <AuthProvider>
                <Story />
              </AuthProvider>
            </ConfigProvider>
          </MemoryRouter>
        </QueryClientProvider>
      );
    },
  ],
  parameters: {
    layout: "fullscreen",
    msw: {
      handlers: createHandlers(mockApiKeyConfig),
    },
  },
};

export default meta;
type Story = StoryObj<typeof LoginPage>;

/** Default login page with API key authentication only. */
export const Default: Story = {};

/** Login page while config is still loading. */
export const Loading: Story = {
  parameters: {
    msw: {
      handlers: [
        http.get("*/admin/v1/ui/config", async () => {
          await new Promise((resolve) => setTimeout(resolve, 999999));
          return HttpResponse.json(mockApiKeyConfig);
        }),
        ...unauthenticatedHandlers,
      ],
    },
  },
};

/** Login page with OIDC provider and API key options. Shows email discovery form and API key form. */
export const WithOidc: Story = {
  parameters: {
    msw: {
      handlers: createHandlers(mockOidcConfig),
    },
  },
};

/** Login page with per-organization SSO discovery and API key fallback. */
export const WithPerOrgSso: Story = {
  parameters: {
    msw: {
      handlers: createHandlers(mockPerOrgSsoConfig),
    },
  },
};

/** IdP mode: email discovery only — the gateway advertises ["session", "per_org_sso"]. */
export const IdpMode: Story = {
  parameters: {
    msw: {
      handlers: createHandlers(mockIdpConfig),
    },
  },
};

/**
 * IdP mode before any org SSO config is enabled. No login flow can succeed
 * yet (discovery has nothing to find), so the page shows setup guidance
 * instead of an email form that would dead-end on every submission.
 */
export const IdpModeNoOrgSso: Story = {
  parameters: {
    msw: {
      handlers: createHandlers(mockIdpNoOrgSsoConfig),
    },
  },
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement);
    await waitFor(() => {
      expect(canvas.getByText(/no identity provider has been configured/i)).toBeInTheDocument();
    });
    expect(canvas.queryByLabelText(/work email/i)).not.toBeInTheDocument();
  },
};

/**
 * Regression: a user returning from per-org SSO holds a session cookie, so
 * /auth/me succeeds. The session probe must run for IdP-mode method sets
 * (["session", "per_org_sso"], no "oidc") and redirect away from the login
 * page instead of bouncing the user back to the email form.
 */
export const IdpModeWithExistingSession: Story = {
  parameters: {
    msw: {
      handlers: [
        http.get("*/admin/v1/ui/config", () => {
          return HttpResponse.json(mockIdpConfig);
        }),
        http.get("*/auth/me", () => {
          return HttpResponse.json({
            external_id: "okta|user-1",
            email: "user@example.com",
            name: "Example User",
            user_id: "11111111-1111-1111-1111-111111111111",
            roles: [],
          });
        }),
      ],
    },
  },
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement);
    await waitFor(() => {
      expect(canvas.queryByText("Loading...")).not.toBeInTheDocument();
    });
    expect(canvas.queryByLabelText(/work email/i)).not.toBeInTheDocument();
    expect(canvas.queryByText(/no authentication methods available/i)).not.toBeInTheDocument();
  },
};

/** Login page with custom branding: title, subtitle, and SSO provider name. */
export const CustomBranding: Story = {
  parameters: {
    msw: {
      handlers: createHandlers(mockBrandedConfig),
    },
  },
};

/** Login page when no authentication methods are configured. Shows a warning message. */
export const NoAuthMethods: Story = {
  parameters: {
    msw: {
      handlers: createHandlers(mockNoAuthConfig),
    },
  },
};

/** Login page when the config endpoint fails. Falls back to default config. */
export const WithError: Story = {
  parameters: {
    msw: {
      handlers: [
        http.get("*/admin/v1/ui/config", () => {
          return HttpResponse.error();
        }),
        ...unauthenticatedHandlers,
      ],
    },
  },
};
