import { useState, useEffect, useCallback } from "react";
import { useForm, Controller } from "react-hook-form";
import { zodResolver } from "@hookform/resolvers/zod";
import { z } from "zod";
import { Wifi } from "lucide-react";

import type {
  DynamicProvider,
  CreateDynamicProvider,
  UpdateDynamicProvider,
  ProviderOwner,
  Organization,
  ConnectivityTestResponse,
} from "@/api/generated/types.gen";
import { dynamicProviderTestCredentials } from "@/api/generated/sdk.gen";
import {
  PROVIDER_TYPES,
  providerNeedsBaseUrl,
  providerNeedsApiKey,
  TestResultDisplay,
  buildConfigFromForm,
  configToFormValues,
} from "@/pages/providers/shared";
import { Button } from "@/components/Button/Button";
import { FormField } from "@/components/FormField/FormField";
import { Input } from "@/components/Input/Input";
import { Modal, ModalHeader, ModalContent, ModalFooter } from "@/components/Modal/Modal";
import { Switch } from "@/components/Switch/Switch";

const createProviderSchema = z
  .object({
    name: z
      .string()
      .min(1, "Name is required")
      .regex(/^[a-z0-9-]+$/, "Name must be lowercase alphanumeric with hyphens only"),
    provider_type: z.string().min(1, "Provider type is required"),
    base_url: z.string().default(""),
    api_key: z.string().optional(),
    models: z.string().optional(),
    org_id: z.string().min(1, "Organization is required"),
    is_enabled: z.boolean(),
    // Bedrock fields (static credentials only — server env access not allowed)
    aws_region: z.string().optional(),
    aws_access_key_id: z.string().optional(),
    aws_secret_access_key: z.string().optional(),
    // Vertex fields (OAuth/ADC service-account auth)
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

type ProviderFormValues = z.input<typeof createProviderSchema>;

const defaultValues: ProviderFormValues = {
  name: "",
  provider_type: "open_ai",
  base_url: "https://api.openai.com/v1",
  api_key: "",
  models: "",
  org_id: "",
  is_enabled: true,
  aws_region: "",
  aws_access_key_id: "",
  aws_secret_access_key: "",
  gcp_project: "",
  gcp_region: "",
  gcp_sa_json: "",
};

/** Build a CreateDynamicProvider body from form values */
function formToCreateBody(
  data: ProviderFormValues,
  ownerOverride?: ProviderOwner
): CreateDynamicProvider {
  const models = (data.models || "")
    .split(",")
    .map((m) => m.trim())
    .filter(Boolean);
  const config = buildConfigFromForm(data);
  const owner: ProviderOwner = ownerOverride ?? { type: "organization", org_id: data.org_id };
  return {
    name: data.name,
    provider_type: data.provider_type,
    base_url: data.base_url ?? "",
    api_key: data.api_key || null,
    config: config ?? undefined,
    models,
    owner,
  };
}

export interface ProviderFormModalProps {
  isOpen: boolean;
  onClose: () => void;
  onCreateSubmit: (data: CreateDynamicProvider) => void;
  onEditSubmit: (data: UpdateDynamicProvider) => void;
  isLoading?: boolean;
  editingProvider?: DynamicProvider | null;
  organizations?: Organization[];
  /** When set, bypasses the organization selector and uses this owner for new providers */
  ownerOverride?: ProviderOwner;
}

export function ProviderFormModal({
  isOpen,
  onClose,
  onCreateSubmit,
  onEditSubmit,
  isLoading,
  editingProvider,
  organizations,
  ownerOverride,
}: ProviderFormModalProps) {
  const isEditing = !!editingProvider;
  const [credTestResult, setCredTestResult] = useState<ConnectivityTestResponse | null>(null);
  const [isTestingCreds, setIsTestingCreds] = useState(false);

  const form = useForm<ProviderFormValues>({
    resolver: zodResolver(createProviderSchema),
    defaultValues,
  });

  const providerType = form.watch("provider_type");

  // Auto-fill base URL when provider type changes (only for new providers)
  const handleProviderTypeChange = (newType: string) => {
    const match = PROVIDER_TYPES.find((p) => p.value === newType);
    if (match) {
      form.setValue("base_url", match.baseUrl, { shouldValidate: false });
    }
    // Reset provider-specific fields
    form.setValue("aws_region", "");
    form.setValue("gcp_project", "");
    form.setValue("gcp_region", "");
    setCredTestResult(null);
  };

  // Reset form when modal opens with different data
  useEffect(() => {
    if (isOpen) {
      setCredTestResult(null);
      setIsTestingCreds(false);
      if (editingProvider) {
        const configValues = configToFormValues(
          editingProvider.config as Record<string, unknown> | null | undefined,
          editingProvider.provider_type
        );
        form.reset({
          name: editingProvider.name,
          provider_type: editingProvider.provider_type,
          base_url: editingProvider.base_url,
          api_key: "",
          models: editingProvider.models.join(", "),
          org_id: ownerOverride ? "override" : "",
          is_enabled: editingProvider.is_enabled,
          ...configValues,
        });
      } else {
        form.reset({
          ...defaultValues,
          org_id: ownerOverride ? "override" : "",
        });
      }
    }
  }, [isOpen, editingProvider, form, ownerOverride]);

  const handleTestCredentials = useCallback(async () => {
    const valid = await form.trigger();
    if (!valid) return;

    const data = form.getValues();
    const body = formToCreateBody(data, ownerOverride);

    setIsTestingCreds(true);
    setCredTestResult(null);

    try {
      const { data: result } = await dynamicProviderTestCredentials({
        body,
        throwOnError: true,
      });
      setCredTestResult(result);
    } catch (e) {
      setCredTestResult({
        status: "error",
        message: String(e),
        latency_ms: null,
      });
    } finally {
      setIsTestingCreds(false);
    }
  }, [form, ownerOverride]);

  const handleSubmit = form.handleSubmit((data) => {
    const models = (data.models || "")
      .split(",")
      .map((m: string) => m.trim())
      .filter(Boolean);

    const config = buildConfigFromForm(data);

    if (isEditing) {
      const body: UpdateDynamicProvider = {
        base_url: providerNeedsBaseUrl(data.provider_type) ? data.base_url || null : null,
        api_key: data.api_key || null,
        config: config ?? undefined,
        models,
        is_enabled: data.is_enabled,
      };
      onEditSubmit(body);
    } else {
      onCreateSubmit(formToCreateBody(data, ownerOverride));
    }
  });

  const showBaseUrl = providerNeedsBaseUrl(providerType);
  const showApiKey = providerNeedsApiKey(providerType);
  const showBedrockFields = providerType === "bedrock";
  const showVertexFields = providerType === "vertex";

  return (
    <Modal open={isOpen} onClose={onClose}>
      <form onSubmit={handleSubmit}>
        <ModalHeader>{isEditing ? "Edit Provider" : "Add Dynamic Provider"}</ModalHeader>
        <ModalContent>
          <div className="space-y-4">
            {!isEditing && (
              <>
                <FormField
                  label="Name"
                  htmlFor="provider-name"
                  required
                  helpText="Used as a prefix in model names (e.g., my-provider/gpt-4)"
                  error={form.formState.errors.name?.message}
                >
                  <Input id="provider-name" {...form.register("name")} placeholder="my-openai" />
                </FormField>

                <FormField
                  label="Provider Type"
                  htmlFor="provider-type"
                  required
                  error={form.formState.errors.provider_type?.message}
                >
                  <select
                    id="provider-type"
                    {...form.register("provider_type", {
                      onChange: (e) => handleProviderTypeChange(e.target.value),
                    })}
                    className="w-full rounded-md border border-input bg-background px-3 py-2 text-sm"
                  >
                    {PROVIDER_TYPES.map((type) => (
                      <option key={type.value} value={type.value}>
                        {type.label}
                      </option>
                    ))}
                  </select>
                </FormField>
              </>
            )}

            {showBaseUrl && (
              <FormField
                label="Base URL"
                htmlFor="provider-base-url"
                required={!isEditing}
                helpText="Change this to use any API-compatible endpoint"
                error={form.formState.errors.base_url?.message}
              >
                <Input
                  id="provider-base-url"
                  {...form.register("base_url")}
                  placeholder="https://api.openai.com/v1"
                />
              </FormField>
            )}

            {showApiKey && (
              <FormField
                label="API Key"
                htmlFor="provider-api-key"
                helpText={
                  isEditing ? "Leave empty to keep the existing key" : "Your provider API key"
                }
                error={form.formState.errors.api_key?.message}
              >
                <Input
                  id="provider-api-key"
                  type="password"
                  autoComplete="off"
                  {...form.register("api_key")}
                  placeholder={isEditing ? "Leave empty to keep existing" : "sk-..."}
                />
              </FormField>
            )}

            {/* Bedrock-specific fields (static credentials only) */}
            {showBedrockFields && (
              <>
                <FormField
                  label="AWS Region"
                  htmlFor="aws-region"
                  required
                  error={form.formState.errors.aws_region?.message}
                >
                  <Input id="aws-region" {...form.register("aws_region")} placeholder="us-east-1" />
                </FormField>

                <FormField
                  label="Access Key ID"
                  htmlFor="aws-access-key"
                  helpText={isEditing ? "Leave empty to keep existing" : "AWS access key ID"}
                >
                  <Input
                    id="aws-access-key"
                    type="password"
                    autoComplete="off"
                    {...form.register("aws_access_key_id")}
                    placeholder="AKIA..."
                  />
                </FormField>
                <FormField
                  label="Secret Access Key"
                  htmlFor="aws-secret-key"
                  helpText={isEditing ? "Leave empty to keep existing" : "AWS secret access key"}
                >
                  <Input
                    id="aws-secret-key"
                    type="password"
                    autoComplete="off"
                    {...form.register("aws_secret_access_key")}
                    placeholder="Secret key"
                  />
                </FormField>
              </>
            )}

            {/* Vertex-specific fields (OAuth/ADC service-account auth) */}
            {showVertexFields && (
              <>
                <FormField
                  label="GCP Project"
                  htmlFor="gcp-project"
                  required
                  error={form.formState.errors.gcp_project?.message}
                >
                  <Input
                    id="gcp-project"
                    {...form.register("gcp_project")}
                    placeholder="my-gcp-project"
                  />
                </FormField>

                <FormField
                  label="GCP Region"
                  htmlFor="gcp-region"
                  required
                  error={form.formState.errors.gcp_region?.message}
                >
                  <Input
                    id="gcp-region"
                    {...form.register("gcp_region")}
                    placeholder="us-central1"
                  />
                </FormField>

                <FormField
                  label="Service Account JSON"
                  htmlFor="gcp-sa-json"
                  helpText="Secret reference to service account JSON (e.g., secret:gcp-sa)"
                >
                  <Input
                    id="gcp-sa-json"
                    type="password"
                    autoComplete="off"
                    {...form.register("gcp_sa_json")}
                    placeholder="secret:gcp-sa"
                  />
                </FormField>
              </>
            )}

            <FormField
              label="Supported Models"
              htmlFor="provider-models"
              helpText="Comma-separated list of model names (leave empty for all)"
              error={form.formState.errors.models?.message}
            >
              <Input
                id="provider-models"
                {...form.register("models")}
                placeholder="gpt-4o, gpt-4o-mini"
              />
            </FormField>

            {!isEditing && !ownerOverride && organizations && (
              <FormField
                label="Organization"
                htmlFor="provider-org"
                required
                error={form.formState.errors.org_id?.message}
              >
                <select
                  id="provider-org"
                  {...form.register("org_id")}
                  className="w-full rounded-md border border-input bg-background px-3 py-2 text-sm"
                >
                  <option value="">Select organization...</option>
                  {organizations.map((org) => (
                    <option key={org.id} value={org.id}>
                      {org.name}
                    </option>
                  ))}
                </select>
              </FormField>
            )}

            {isEditing && (
              <Controller
                name="is_enabled"
                control={form.control}
                render={({ field: { value, onChange, ...field } }) => (
                  <Switch
                    label="Enabled"
                    checked={value}
                    onChange={(e) => onChange(e.target.checked)}
                    {...field}
                  />
                )}
              />
            )}

            {/* Test credentials result */}
            <TestResultDisplay isTesting={isTestingCreds} testResult={credTestResult} />
          </div>
        </ModalContent>
        <ModalFooter>
          <Button type="button" variant="ghost" onClick={onClose}>
            Cancel
          </Button>
          <Button
            type="button"
            variant="outline"
            onClick={handleTestCredentials}
            isLoading={isTestingCreds}
          >
            <Wifi className="h-4 w-4 mr-2" />
            Test
          </Button>
          <Button type="submit" isLoading={isLoading}>
            {isEditing ? "Save" : "Create"}
          </Button>
        </ModalFooter>
      </form>
    </Modal>
  );
}
