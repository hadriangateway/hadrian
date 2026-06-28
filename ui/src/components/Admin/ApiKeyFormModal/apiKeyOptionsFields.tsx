import { useState } from "react";
import { Controller, type Control, type UseFormRegister, type FieldErrors } from "react-hook-form";
import { ChevronDown, Info } from "lucide-react";

import type { BudgetPeriod } from "@/api/generated/types.gen";
import { FormField } from "@/components/FormField/FormField";
import { Input } from "@/components/Input/Input";
import { Select } from "@/components/Select/Select";
import { Textarea } from "@/components/Textarea/Textarea";
import { Tooltip, TooltipContent, TooltipTrigger } from "@/components/Tooltip/Tooltip";
import { cn } from "@/utils/cn";

import { buildSovereigntyRequirements, SovereigntyFormFields } from "./sovereigntyFields";

/**
 * API key scopes available for selection. Mirrors `ApiKeyScope` in the
 * backend (`src/models/api_key.rs`); keep in sync.
 *
 * Each scope grants access to a specific group of OpenAI-compatible
 * endpoints. `description` is shown in the consent / create-key UIs so the
 * user knows what they're granting.
 */
export const API_KEY_SCOPES = [
  { value: "chat", label: "Chat", description: "Chat completions and Responses API" },
  { value: "completions", label: "Completions", description: "Legacy text completions" },
  { value: "embeddings", label: "Embeddings", description: "Generate vector embeddings" },
  { value: "images", label: "Images", description: "Image generation, edits, variations" },
  { value: "videos", label: "Videos", description: "Video generation, remix, edits, characters" },
  { value: "audio", label: "Audio", description: "Speech, transcription, translation" },
  {
    value: "files",
    label: "Files",
    description: "Upload, list, delete files and vector stores",
  },
  { value: "models", label: "Models", description: "List available models" },
  { value: "admin", label: "Admin", description: "Full access to /admin endpoints" },
];

const MODEL_PATTERN_REGEX = /^[a-zA-Z0-9][a-zA-Z0-9\-._/]*\*?$/;

export function validateModelPatterns(value: string | undefined): boolean {
  if (!value || value.trim() === "") return true;
  return value
    .split(",")
    .map((p) => p.trim())
    .filter(Boolean)
    .every((p) => MODEL_PATTERN_REGEX.test(p));
}

const IPV4_REGEX = /^(\d{1,3}\.){3}\d{1,3}(\/\d{1,2})?$/;

function isValidIPv4(ip: string): boolean {
  const cidrMatch = ip.match(/^(.+)\/(\d+)$/);
  const address = cidrMatch ? cidrMatch[1] : ip;
  const prefix = cidrMatch ? parseInt(cidrMatch[2], 10) : null;
  if (prefix !== null && (prefix < 0 || prefix > 32)) return false;
  if (!IPV4_REGEX.test(ip)) return false;
  const octets = address.split(".").map((o) => parseInt(o, 10));
  return octets.every((o) => o >= 0 && o <= 255);
}

function isValidIPv6(ip: string): boolean {
  const cidrMatch = ip.match(/^(.+)\/(\d+)$/);
  const address = cidrMatch ? cidrMatch[1] : ip;
  const prefix = cidrMatch ? parseInt(cidrMatch[2], 10) : null;
  if (prefix !== null && (prefix < 0 || prefix > 128)) return false;
  if (!/^[0-9a-fA-F:]+$/.test(address)) return false;
  if (address.includes(":::")) return false;
  const doubleColonCount = (address.match(/::/g) || []).length;
  if (doubleColonCount > 1) return false;
  const groups = address.split(":");
  if (address.includes("::")) {
    const nonEmptyGroupCount = groups.filter((g) => g !== "").length;
    if (nonEmptyGroupCount > 7) return false;
  } else {
    if (groups.length !== 8) return false;
  }
  const nonEmptyGroups = groups.filter((g) => g !== "");
  return nonEmptyGroups.every((g) => g.length >= 1 && g.length <= 4 && /^[0-9a-fA-F]+$/.test(g));
}

export function validateCidrNotation(value: string | undefined): boolean {
  if (!value || value.trim() === "") return true;
  return value
    .split("\n")
    .map((e) => e.trim())
    .filter(Boolean)
    .every((entry) => isValidIPv4(entry) || isValidIPv6(entry));
}

/**
 * Shape of the form values backing both the self-service "Create API Key"
 * modal and the OAuth PKCE consent page. Strings (rather than numbers /
 * arrays) are used at the form level so they pair naturally with HTML inputs.
 *
 * `name` is required because both flows need a label on the issued key —
 * the consent page seeds it with the requesting app's name.
 */
export interface ApiKeyOptionsFormValues {
  name: string;
  budget_limit_cents?: string;
  budget_period?: "" | "daily" | "monthly";
  expires_at?: string;
  scopes?: string[];
  allowed_models?: string;
  ip_allowlist?: string;
  rate_limit_rpm?: string;
  rate_limit_tpm?: string;
  sov_inference_countries?: string;
  sov_blocked_countries?: string;
  sov_certifications?: string;
  sov_licenses?: string;
  sov_require_on_prem?: boolean;
  sov_require_open_weights?: boolean;
}

/**
 * Convert form values into the wire-format options applied to a freshly
 * issued API key. Returns the same shape used by both
 * `CreateSelfServiceApiKey` and `OAuthKeyOptions`.
 */
export function buildApiKeyOptionsPayload(data: ApiKeyOptionsFormValues): {
  name: string;
  budget_limit_cents: number | null;
  budget_period: BudgetPeriod | null;
  expires_at: string | null;
  scopes: string[] | null;
  allowed_models: string[] | null;
  ip_allowlist: string[] | null;
  rate_limit_rpm: number | null;
  rate_limit_tpm: number | null;
  sovereignty_requirements: Record<string, unknown> | undefined;
} {
  const allowedModels = data.allowed_models
    ? data.allowed_models
        .split(",")
        .map((m) => m.trim())
        .filter(Boolean)
    : null;

  const ipAllowlist = data.ip_allowlist
    ? data.ip_allowlist
        .split("\n")
        .map((ip) => ip.trim())
        .filter(Boolean)
    : null;

  return {
    name: data.name.trim(),
    budget_limit_cents: data.budget_limit_cents
      ? Math.round(parseFloat(data.budget_limit_cents) * 100)
      : null,
    budget_period: (data.budget_period as BudgetPeriod) || null,
    expires_at: data.expires_at || null,
    scopes: data.scopes && data.scopes.length > 0 ? data.scopes : null,
    allowed_models: allowedModels && allowedModels.length > 0 ? allowedModels : null,
    ip_allowlist: ipAllowlist && ipAllowlist.length > 0 ? ipAllowlist : null,
    rate_limit_rpm: data.rate_limit_rpm ? parseInt(data.rate_limit_rpm) : null,
    rate_limit_tpm: data.rate_limit_tpm ? parseInt(data.rate_limit_tpm) : null,
    sovereignty_requirements: buildSovereigntyRequirements(data),
  };
}

interface ApiKeyOptionsFieldsProps {
  register: UseFormRegister<ApiKeyOptionsFormValues>;
  control: Control<ApiKeyOptionsFormValues>;
  errors: FieldErrors<ApiKeyOptionsFormValues>;
  selectedScopes: string[];
  /** Prefix used for input ids so multiple instances on a page don't collide. */
  idPrefix: string;
  /** Render the name field. Defaults to true. */
  showName?: boolean;
  /** Whether the name field is required (shows asterisk). Defaults to true. */
  nameRequired?: boolean;
  /** Placeholder for the name field. */
  namePlaceholder?: string;
  /** Whether advanced settings start expanded. Defaults to false. */
  advancedDefaultOpen?: boolean;
}

/**
 * The shared field set that backs both the "Create API Key" modal and the
 * OAuth PKCE consent page. Render it inside an existing react-hook-form
 * form; the caller owns the schema, defaults, and submission.
 */
export function ApiKeyOptionsFields({
  register,
  control,
  errors,
  selectedScopes,
  idPrefix,
  showName = true,
  nameRequired = true,
  namePlaceholder = "My API Key",
  advancedDefaultOpen = false,
}: ApiKeyOptionsFieldsProps) {
  const [advancedOpen, setAdvancedOpen] = useState(advancedDefaultOpen);

  return (
    <div className="space-y-4">
      {showName && (
        <FormField
          label="Name"
          htmlFor={`${idPrefix}-name`}
          required={nameRequired}
          error={errors.name?.message}
        >
          <Input id={`${idPrefix}-name`} {...register("name")} placeholder={namePlaceholder} />
        </FormField>
      )}

      <div className="grid grid-cols-2 gap-4">
        <FormField
          label="Budget Limit ($)"
          htmlFor={`${idPrefix}-budget`}
          error={errors.budget_limit_cents?.message}
        >
          <Input
            id={`${idPrefix}-budget`}
            type="number"
            min="0"
            step="0.01"
            {...register("budget_limit_cents")}
            placeholder="100.00"
          />
        </FormField>
        <FormField
          label="Budget Period"
          htmlFor={`${idPrefix}-period`}
          error={errors.budget_period?.message}
        >
          <Controller
            name="budget_period"
            control={control}
            render={({ field }) => (
              <select
                id={`${idPrefix}-period`}
                {...field}
                className="w-full rounded-md border border-input bg-background px-3 py-2 text-sm"
              >
                <option value="">No period</option>
                <option value="daily">Daily</option>
                <option value="monthly">Monthly</option>
              </select>
            )}
          />
        </FormField>
      </div>

      <FormField
        label="Expires At"
        htmlFor={`${idPrefix}-expires`}
        helpText="Leave empty for no expiration"
        error={errors.expires_at?.message}
      >
        <Input id={`${idPrefix}-expires`} type="datetime-local" {...register("expires_at")} />
      </FormField>

      <div className="border-t pt-4">
        <button
          type="button"
          className="flex w-full items-center justify-between text-sm font-medium text-muted-foreground hover:text-foreground"
          onClick={() => setAdvancedOpen(!advancedOpen)}
          aria-expanded={advancedOpen}
        >
          Advanced Settings
          <ChevronDown
            className={cn("h-4 w-4 transition-transform", advancedOpen && "rotate-180")}
            aria-hidden="true"
          />
        </button>

        <div
          className={cn(
            "overflow-hidden transition-all duration-200",
            advancedOpen ? "max-h-[1200px] opacity-100 mt-4" : "max-h-0 opacity-0"
          )}
        >
          <div className="grid grid-cols-2 gap-x-6 gap-y-4">
            <div className="col-span-2">
              <FormField
                label={
                  <span className="flex items-center gap-1">
                    Permission Scopes
                    <Tooltip>
                      <TooltipTrigger asChild>
                        <Info className="h-3.5 w-3.5 text-muted-foreground cursor-help" />
                      </TooltipTrigger>
                      <TooltipContent className="max-w-xs">
                        <p>Restrict which API endpoints this key can access.</p>
                        <p className="mt-1 text-xs text-muted-foreground">
                          Leave empty for full access to all endpoints.
                        </p>
                      </TooltipContent>
                    </Tooltip>
                  </span>
                }
                htmlFor={`${idPrefix}-scopes`}
                helpText={
                  selectedScopes.length > 0
                    ? `${selectedScopes.length} scope${selectedScopes.length === 1 ? "" : "s"} selected`
                    : "No restrictions (full access)"
                }
              >
                <Controller
                  name="scopes"
                  control={control}
                  render={({ field }) => (
                    <Select
                      multiple
                      options={API_KEY_SCOPES}
                      value={field.value || []}
                      onChange={field.onChange}
                      placeholder="Select scopes..."
                      searchable
                    />
                  )}
                />
              </FormField>
            </div>

            <div className="col-span-2">
              <FormField
                label="Model Restrictions"
                htmlFor={`${idPrefix}-models`}
                helpText="Comma-separated. Supports wildcards: gpt-4, claude-*, anthropic/*"
                error={errors.allowed_models?.message}
              >
                <Input
                  id={`${idPrefix}-models`}
                  {...register("allowed_models")}
                  placeholder="gpt-4, claude-*, anthropic/claude-3-*"
                />
              </FormField>
            </div>

            <FormField
              label="Requests/min"
              htmlFor={`${idPrefix}-rpm`}
              helpText="Override global limit"
              error={errors.rate_limit_rpm?.message}
            >
              <Input
                id={`${idPrefix}-rpm`}
                type="number"
                min="1"
                {...register("rate_limit_rpm")}
                placeholder="Default"
              />
            </FormField>
            <FormField
              label="Tokens/min"
              htmlFor={`${idPrefix}-tpm`}
              helpText="Override global limit"
              error={errors.rate_limit_tpm?.message}
            >
              <Input
                id={`${idPrefix}-tpm`}
                type="number"
                min="1"
                {...register("rate_limit_tpm")}
                placeholder="Default"
              />
            </FormField>

            <div className="col-span-2">
              <FormField
                label="IP Allowlist"
                htmlFor={`${idPrefix}-ips`}
                helpText="One IP or CIDR per line. Leave empty to allow all IPs."
                error={errors.ip_allowlist?.message}
              >
                <Textarea
                  id={`${idPrefix}-ips`}
                  {...register("ip_allowlist")}
                  placeholder="192.168.1.0/24&#10;10.0.0.1&#10;2001:db8::/32"
                  className="font-mono text-xs min-h-[80px]"
                  rows={3}
                />
              </FormField>
            </div>

            <SovereigntyFormFields register={register} idPrefix={idPrefix} />
          </div>
        </div>
      </div>
    </div>
  );
}
