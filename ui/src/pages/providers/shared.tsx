import { Loader2, CheckCircle2, XCircle } from "lucide-react";
import { z } from "zod";
import type { ConnectivityTestResponse } from "@/api/generated/types.gen";

// Supported provider types with default base URLs
export const PROVIDER_TYPES = [
  { value: "open_ai", label: "OpenAI", baseUrl: "https://api.openai.com/v1", needsBaseUrl: true },
  {
    value: "anthropic",
    label: "Anthropic",
    baseUrl: "https://api.anthropic.com",
    needsBaseUrl: true,
  },
  {
    value: "azure_open_ai",
    label: "Azure OpenAI",
    baseUrl: "https://YOUR_RESOURCE.openai.azure.com",
    needsBaseUrl: true,
  },
  { value: "bedrock", label: "AWS Bedrock", baseUrl: "", needsBaseUrl: false },
  { value: "vertex", label: "Google Vertex AI", baseUrl: "", needsBaseUrl: false },
  { value: "gemini", label: "Google Gemini", baseUrl: "", needsBaseUrl: false },
] as const;

export type ProviderTypeValue = (typeof PROVIDER_TYPES)[number]["value"];

export function providerNeedsBaseUrl(type: string): boolean {
  const match = PROVIDER_TYPES.find((p) => p.value === type);
  return match?.needsBaseUrl ?? true;
}

export function providerNeedsApiKey(type: string): boolean {
  // Bedrock uses AWS credentials; Vertex uses OAuth/ADC service-account auth.
  return type !== "bedrock" && type !== "vertex";
}

export function getProviderTypeLabel(providerType: string): string {
  const match = PROVIDER_TYPES.find((p) => p.value === providerType);
  if (match) return match.label;
  if (providerType === "openai" || providerType === "openai_compatible") return "OpenAI";
  if (providerType === "azure_openai") return "Azure OpenAI";
  if (providerType === "bedrock") return "AWS Bedrock";
  if (providerType === "vertex") return "Google Vertex AI";
  if (providerType === "gemini") return "Google Gemini";
  return providerType;
}

// -- Test Result Display --

export function TestResultDisplay({
  isTesting,
  testResult,
}: {
  isTesting: boolean;
  testResult?: ConnectivityTestResponse | null;
}) {
  if (!isTesting && !testResult) return null;
  return (
    <div className="mt-2">
      {isTesting ? (
        <div className="flex items-center gap-1.5 text-xs text-muted-foreground">
          <Loader2 className="h-3 w-3 animate-spin" />
          Testing connection...
        </div>
      ) : testResult?.status === "ok" ? (
        <div className="flex items-center gap-1.5 text-xs text-green-600 dark:text-green-400">
          <CheckCircle2 className="h-3 w-3" />
          {testResult.message}
          {testResult.latency_ms != null && (
            <span className="text-muted-foreground">({testResult.latency_ms}ms)</span>
          )}
        </div>
      ) : (
        <div className="flex items-center gap-1.5 text-xs text-destructive">
          <XCircle className="h-3 w-3" />
          {testResult?.message ?? "Test failed"}
        </div>
      )}
    </div>
  );
}

// -- Form Schema & Types --

export const createProviderSchema = z
  .object({
    name: z
      .string()
      .min(1, "Name is required")
      .regex(/^[a-z0-9-]+$/, "Name must be lowercase alphanumeric with hyphens only"),
    provider_type: z.string().min(1, "Provider type is required"),
    base_url: z.string().default(""),
    api_key: z.string().optional(),
    models: z.string().optional(),
    is_enabled: z.boolean(),
    aws_region: z.string().optional(),
    aws_access_key_id: z.string().optional(),
    aws_secret_access_key: z.string().optional(),
    gcp_project: z.string().optional(),
    gcp_region: z.string().optional(),
    gcp_sa_json: z.string().optional(),
  })
  .superRefine((data, ctx) => {
    if (providerNeedsBaseUrl(data.provider_type) && !data.base_url) {
      ctx.addIssue({
        code: "custom",
        message: "Base URL is required for this provider type",
        path: ["base_url"],
      });
    }
    if (data.provider_type === "bedrock" && !data.aws_region) {
      ctx.addIssue({
        code: "custom",
        message: "AWS region is required",
        path: ["aws_region"],
      });
    }
    if (data.provider_type === "vertex" && !data.gcp_project) {
      ctx.addIssue({
        code: "custom",
        message: "GCP project is required",
        path: ["gcp_project"],
      });
    }
    if (data.provider_type === "vertex" && !data.gcp_region) {
      ctx.addIssue({
        code: "custom",
        message: "GCP region is required",
        path: ["gcp_region"],
      });
    }
  });

export type ProviderFormValues = z.input<typeof createProviderSchema>;

export const defaultFormValues: ProviderFormValues = {
  name: "",
  provider_type: "open_ai",
  base_url: "https://api.openai.com/v1",
  api_key: "",
  models: "",
  is_enabled: true,
  aws_region: "",
  aws_access_key_id: "",
  aws_secret_access_key: "",
  gcp_project: "",
  gcp_region: "",
  gcp_sa_json: "",
};

export function buildConfigFromForm(data: ProviderFormValues): Record<string, unknown> | null {
  if (data.provider_type === "bedrock") {
    const credentials: Record<string, unknown> = { type: "static" };
    if (data.aws_access_key_id) credentials.access_key_id_ref = data.aws_access_key_id;
    if (data.aws_secret_access_key) credentials.secret_access_key_ref = data.aws_secret_access_key;
    return { region: data.aws_region, credentials };
  }

  if (data.provider_type === "vertex") {
    const credentials: Record<string, unknown> = { type: "service_account_json" };
    if (data.gcp_sa_json) credentials.json_ref = data.gcp_sa_json;
    return { project: data.gcp_project, region: data.gcp_region, credentials };
  }

  return null;
}

export function configToFormValues(
  config: Record<string, unknown> | null | undefined,
  providerType: string
): Partial<ProviderFormValues> {
  if (!config) return {};

  if (providerType === "bedrock") {
    const creds = config.credentials as Record<string, unknown> | undefined;
    return {
      aws_region: (config.region as string) ?? "",
      aws_access_key_id: (creds?.access_key_id_ref as string) ?? "",
      aws_secret_access_key: (creds?.secret_access_key_ref as string) ?? "",
    };
  }

  if (providerType === "vertex") {
    return {
      gcp_project: (config.project as string) ?? "",
      gcp_region: (config.region as string) ?? "",
      gcp_sa_json:
        ((config.credentials as Record<string, unknown> | undefined)?.json_ref as string) ?? "",
    };
  }

  return {};
}

// -- Provider Color Mapping --

export interface ProviderColorEntry {
  /** Solid background class for small indicators (e.g., sidebar dots) */
  solid: string;
  /** Badge-style classes with muted background and text color */
  badge: string;
}

/**
 * Canonical color mapping for known providers.
 * Both `solid` and `badge` strings must be complete Tailwind classes for JIT detection.
 */
export const PROVIDER_COLORS: Record<string, ProviderColorEntry> = {
  anthropic: {
    solid: "bg-orange-500",
    badge: "bg-orange-500/10 text-orange-800 dark:text-orange-400",
  },
  openai: {
    solid: "bg-green-500",
    badge: "bg-green-500/10 text-green-800 dark:text-green-400",
  },
  google: {
    solid: "bg-blue-500",
    badge: "bg-blue-500/10 text-blue-700 dark:text-blue-400",
  },
  meta: {
    solid: "bg-purple-500",
    badge: "bg-purple-500/10 text-purple-700 dark:text-purple-400",
  },
  mistral: {
    solid: "bg-cyan-500",
    badge: "bg-cyan-500/10 text-cyan-700 dark:text-cyan-400",
  },
  cohere: {
    solid: "bg-pink-500",
    badge: "bg-pink-500/10 text-pink-700 dark:text-pink-400",
  },
  deepseek: {
    solid: "bg-indigo-500",
    badge: "bg-indigo-500/10 text-indigo-700 dark:text-indigo-400",
  },
  qwen: {
    solid: "bg-teal-500",
    badge: "bg-teal-500/10 text-teal-700 dark:text-teal-400",
  },
  openrouter: {
    solid: "bg-violet-500",
    badge: "bg-violet-500/10 text-violet-700 dark:text-violet-400",
  },
  test: {
    solid: "bg-gray-500",
    badge: "bg-gray-500/10 text-gray-700 dark:text-gray-400",
  },
  browser: {
    solid: "bg-sky-500",
    badge: "bg-sky-500/10 text-sky-700 dark:text-sky-400",
  },
};
