import { useState, useEffect, useCallback } from "react";
import {
  Plus,
  Server,
  Calendar,
  Pencil,
  Trash2,
  Wifi,
  WifiOff,
  Settings2,
  MoreHorizontal,
} from "lucide-react";
import { useQuery, useMutation, useQueryClient } from "@tanstack/react-query";
import { useForm, Controller } from "react-hook-form";
import { zodResolver } from "@hookform/resolvers/zod";

import {
  meProvidersListOptions,
  meProvidersCreateMutation,
  meProvidersDeleteMutation,
  meProvidersUpdateMutation,
  meProvidersTestMutation,
  meBuiltInProvidersListOptions,
} from "@/api/generated/@tanstack/react-query.gen";
import { meProvidersTestCredentials } from "@/api/generated/sdk.gen";
import type {
  DynamicProviderResponse,
  CreateSelfServiceProvider,
  UpdateDynamicProvider,
  ConnectivityTestResponse,
  BuiltInProvider,
} from "@/api/generated/types.gen";
import { Button } from "@/components/Button/Button";
import { Badge } from "@/components/Badge/Badge";
import { Card, CardContent } from "@/components/Card/Card";
import { Input } from "@/components/Input/Input";
import { Skeleton } from "@/components/Skeleton/Skeleton";
import { CodeBadge } from "@/components/CodeBadge/CodeBadge";
import { EnabledStatusBadge } from "@/components/Admin";
import { FormField } from "@/components/FormField/FormField";
import { Modal, ModalHeader, ModalContent, ModalFooter } from "@/components/Modal/Modal";
import { Switch } from "@/components/Switch/Switch";
import {
  Dropdown,
  DropdownContent,
  DropdownItem,
  DropdownTrigger,
} from "@/components/Dropdown/Dropdown";
import { useToast } from "@/components/Toast/Toast";
import { useConfirm } from "@/components/ConfirmDialog/ConfirmDialog";
import { formatDateTime } from "@/utils/formatters";
import { formatApiError } from "@/utils/formatApiError";
import {
  PROVIDER_TYPES,
  type ProviderTypeValue,
  providerNeedsBaseUrl,
  providerNeedsApiKey,
  getProviderTypeLabel,
  TestResultDisplay,
  createProviderSchema,
  type ProviderFormValues,
  defaultFormValues,
  buildConfigFromForm,
  configToFormValues,
} from "@/pages/providers/shared";

// -- Provider Card --

function ProviderCard({
  provider,
  onEdit,
  onDelete,
  onTest,
  testResult,
  isTesting,
}: {
  provider: DynamicProviderResponse;
  onEdit: (provider: DynamicProviderResponse) => void;
  onDelete: (provider: DynamicProviderResponse) => void;
  onTest: (provider: DynamicProviderResponse) => void;
  testResult?: ConnectivityTestResponse | null;
  isTesting: boolean;
}) {
  const config = provider.config as Record<string, unknown> | null | undefined;

  return (
    <Card className="h-full">
      <CardContent className="p-4">
        <div className="flex items-start justify-between gap-2">
          <div className="flex items-center gap-2 min-w-0">
            <Server className="h-5 w-5 text-muted-foreground shrink-0" />
            <p className="font-medium truncate">{provider.name}</p>
          </div>
          <div className="flex items-center gap-2 shrink-0">
            <EnabledStatusBadge isEnabled={provider.is_enabled} />
            <Dropdown>
              <DropdownTrigger aria-label="Actions" variant="ghost" className="h-8 w-8 p-0">
                <MoreHorizontal className="h-4.5 w-4.5" />
              </DropdownTrigger>
              <DropdownContent align="end">
                <DropdownItem onClick={() => onEdit(provider)}>
                  <Pencil className="mr-2 h-4 w-4" />
                  Edit
                </DropdownItem>
                <DropdownItem onClick={() => onTest(provider)}>
                  <Wifi className="mr-2 h-4 w-4" />
                  Test Connection
                </DropdownItem>
                <DropdownItem className="text-destructive" onClick={() => onDelete(provider)}>
                  <Trash2 className="mr-2 h-4 w-4" />
                  Delete
                </DropdownItem>
              </DropdownContent>
            </Dropdown>
          </div>
        </div>

        <div className="mt-2 flex items-center gap-2 flex-wrap">
          <Badge variant="outline">{getProviderTypeLabel(provider.provider_type)}</Badge>
          {config?.region ? (
            <CodeBadge className="text-xs">{String(config.region)}</CodeBadge>
          ) : null}
        </div>

        {provider.models.length > 0 && (
          <div className="mt-2 flex flex-wrap items-center gap-1.5">
            {provider.models.slice(0, 5).map((model) => (
              <Badge key={model} variant="secondary" className="text-xs">
                {model}
              </Badge>
            ))}
            {provider.models.length > 5 && (
              <Badge variant="secondary" className="text-xs">
                +{provider.models.length - 5} more
              </Badge>
            )}
          </div>
        )}

        <TestResultDisplay isTesting={isTesting} testResult={testResult} />

        <div className="mt-3 flex flex-wrap items-center gap-3 text-xs text-muted-foreground">
          {provider.base_url && (
            <span className="truncate max-w-[200px]" title={provider.base_url}>
              {provider.base_url}
            </span>
          )}
          <span className="flex items-center gap-1">
            <Calendar className="h-3 w-3" />
            {formatDateTime(provider.created_at)}
          </span>
        </div>
      </CardContent>
    </Card>
  );
}

/** Build a CreateSelfServiceProvider from form values */
function formToCreateBody(data: ProviderFormValues): CreateSelfServiceProvider {
  const models = (data.models || "")
    .split(",")
    .map((m) => m.trim())
    .filter(Boolean);
  const config = buildConfigFromForm(data);
  return {
    name: data.name,
    provider_type: data.provider_type,
    base_url: data.base_url ?? "",
    api_key: data.api_key || null,
    config: config ?? undefined,
    models: models.length > 0 ? models : null,
  };
}

function ProviderCardSkeleton() {
  return (
    <Card>
      <CardContent className="p-4">
        <div className="flex items-start justify-between gap-2">
          <div className="flex items-center gap-2">
            <Skeleton className="h-5 w-5 rounded" />
            <Skeleton className="h-5 w-32" />
          </div>
          <Skeleton className="h-5 w-16" />
        </div>
        <div className="mt-2 flex items-center gap-2">
          <Skeleton className="h-5 w-24" />
          <Skeleton className="h-5 w-20" />
        </div>
        <div className="mt-3 flex gap-3">
          <Skeleton className="h-3 w-32" />
          <Skeleton className="h-3 w-24" />
        </div>
      </CardContent>
    </Card>
  );
}

// -- Built-in Provider Card --

function BuiltInProviderCard({ provider }: { provider: BuiltInProvider }) {
  return (
    <Card className="h-full">
      <CardContent className="p-4">
        <div className="flex items-start justify-between gap-2">
          <div className="flex items-center gap-2 min-w-0">
            <Settings2 className="h-5 w-5 text-muted-foreground shrink-0" />
            <p className="font-medium truncate">{provider.name}</p>
          </div>
          <Badge variant="outline" className="text-xs shrink-0">
            Built-in
          </Badge>
        </div>
        <div className="mt-2">
          <Badge variant="outline">{getProviderTypeLabel(provider.provider_type)}</Badge>
        </div>
        {provider.base_url && (
          <div className="mt-3 text-xs text-muted-foreground truncate" title={provider.base_url}>
            {provider.base_url}
          </div>
        )}
      </CardContent>
    </Card>
  );
}

// -- Provider Form Modal --

function SelfServiceProviderFormModal({
  isOpen,
  onClose,
  onCreateSubmit,
  onEditSubmit,
  isLoading,
  editingProvider,
}: {
  isOpen: boolean;
  onClose: () => void;
  onCreateSubmit: (data: CreateSelfServiceProvider) => void;
  onEditSubmit: (data: UpdateDynamicProvider) => void;
  isLoading?: boolean;
  editingProvider?: DynamicProviderResponse | null;
}) {
  const isEditing = !!editingProvider;
  const [credTestResult, setCredTestResult] = useState<ConnectivityTestResponse | null>(null);
  const [isTestingCreds, setIsTestingCreds] = useState(false);

  const form = useForm<ProviderFormValues>({
    resolver: zodResolver(createProviderSchema),
    defaultValues: defaultFormValues,
  });

  const providerType = form.watch("provider_type") as ProviderTypeValue;

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
          is_enabled: editingProvider.is_enabled,
          ...configValues,
        });
      } else {
        form.reset(defaultFormValues);
      }
    }
  }, [isOpen, editingProvider, form]);

  const handleTestCredentials = useCallback(async () => {
    // Trigger validation first
    const valid = await form.trigger();
    if (!valid) return;

    const data = form.getValues();
    const body = formToCreateBody(data);

    setIsTestingCreds(true);
    setCredTestResult(null);

    try {
      const { data: result } = await meProvidersTestCredentials({
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
  }, [form]);

  const handleSubmit = form.handleSubmit((data) => {
    const models = (data.models || "")
      .split(",")
      .map((m) => m.trim())
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
      onCreateSubmit(formToCreateBody(data));
    }
  });

  const showBaseUrl = providerNeedsBaseUrl(providerType);
  const showApiKey = providerNeedsApiKey(providerType);
  const showBedrockFields = providerType === "bedrock";
  const showVertexFields = providerType === "vertex";

  return (
    <Modal open={isOpen} onClose={onClose}>
      <form onSubmit={handleSubmit}>
        <ModalHeader>{isEditing ? "Edit Provider" : "Add Provider"}</ModalHeader>
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

// -- Main Page --

export default function MyProvidersPage() {
  const [search, setSearch] = useState("");
  const [isCreateModalOpen, setIsCreateModalOpen] = useState(false);
  const [editingProvider, setEditingProvider] = useState<DynamicProviderResponse | null>(null);
  const [testResults, setTestResults] = useState<Record<string, ConnectivityTestResponse>>({});
  const [testingIds, setTestingIds] = useState<Set<string>>(new Set());
  const { toast } = useToast();
  const confirm = useConfirm();
  const queryClient = useQueryClient();

  const {
    data: providersData,
    isLoading,
    error,
  } = useQuery(meProvidersListOptions({ query: { limit: 100 } }));

  const { data: builtInData } = useQuery(meBuiltInProvidersListOptions());

  const providers = providersData?.data ?? [];
  const builtInProviders = builtInData?.data ?? [];

  const filteredProviders = providers.filter(
    (p) =>
      p.name.toLowerCase().includes(search.toLowerCase()) ||
      p.provider_type.toLowerCase().includes(search.toLowerCase())
  );

  const createMutation = useMutation({
    ...meProvidersCreateMutation(),
    onSuccess: (data) => {
      queryClient.invalidateQueries({ queryKey: [{ _id: "meProvidersList" }] });
      setIsCreateModalOpen(false);
      toast({
        title: "Provider created",
        description: `"${data.name}" has been added.`,
        type: "success",
      });
    },
    onError: (error) => {
      toast({
        title: "Failed to create provider",
        description: formatApiError(error),
        type: "error",
      });
    },
  });

  const updateMutation = useMutation({
    ...meProvidersUpdateMutation(),
    onSuccess: (data) => {
      queryClient.invalidateQueries({ queryKey: [{ _id: "meProvidersList" }] });
      setEditingProvider(null);
      toast({
        title: "Provider updated",
        description: `"${data.name}" has been updated.`,
        type: "success",
      });
    },
    onError: (error) => {
      toast({
        title: "Failed to update provider",
        description: formatApiError(error),
        type: "error",
      });
    },
  });

  const deleteMutation = useMutation({
    ...meProvidersDeleteMutation(),
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: [{ _id: "meProvidersList" }] });
      toast({ title: "Provider deleted", type: "success" });
    },
    onError: (error) => {
      toast({
        title: "Failed to delete provider",
        description: formatApiError(error),
        type: "error",
      });
    },
  });

  const testMutation = useMutation({
    ...meProvidersTestMutation(),
    onSuccess: (data, variables) => {
      const id = variables.path.id;
      setTestResults((prev) => ({ ...prev, [id]: data }));
      setTestingIds((prev) => {
        const next = new Set(prev);
        next.delete(id);
        return next;
      });
    },
    onError: (error, variables) => {
      const id = variables.path.id;
      setTestResults((prev) => ({
        ...prev,
        [id]: { status: "error", message: formatApiError(error), latency_ms: null },
      }));
      setTestingIds((prev) => {
        const next = new Set(prev);
        next.delete(id);
        return next;
      });
    },
  });

  const handleCreateSubmit = (data: CreateSelfServiceProvider) => {
    createMutation.mutate({ body: data });
  };

  const handleEditSubmit = (data: UpdateDynamicProvider) => {
    if (!editingProvider) return;
    updateMutation.mutate({ path: { id: editingProvider.id }, body: data });
  };

  const handleDelete = async (provider: DynamicProviderResponse) => {
    const confirmed = await confirm({
      title: "Delete Provider",
      message: `Are you sure you want to delete "${provider.name}"? This action cannot be undone.`,
      confirmLabel: "Delete",
      variant: "destructive",
    });
    if (confirmed) {
      deleteMutation.mutate({ path: { id: provider.id } });
    }
  };

  const handleTest = (provider: DynamicProviderResponse) => {
    setTestingIds((prev) => new Set(prev).add(provider.id));
    setTestResults((prev) => {
      const next = { ...prev };
      delete next[provider.id];
      return next;
    });
    testMutation.mutate({ path: { id: provider.id } });
  };

  const enabledCount = providers.filter((p) => p.is_enabled).length;
  const disabledCount = providers.filter((p) => !p.is_enabled).length;

  return (
    <div className="p-6 max-w-6xl mx-auto">
      {/* Header */}
      <div className="flex flex-col gap-4 sm:flex-row sm:items-center sm:justify-between mb-6">
        <div>
          <h1 className="text-2xl font-semibold">Providers</h1>
          <p className="text-sm text-muted-foreground mt-1">
            Manage LLM providers and API keys for routing requests
          </p>
        </div>
        <Button onClick={() => setIsCreateModalOpen(true)}>
          <Plus className="h-4 w-4 mr-2" />
          Add Provider
        </Button>
      </div>

      {/* Built-in providers section */}
      {builtInProviders.length > 0 && (
        <div className="mb-8">
          <h2 className="text-lg font-medium mb-3">Built-in Providers</h2>
          <p className="text-sm text-muted-foreground mb-4">
            Configured in the gateway deployment. These are available to all users.
          </p>
          <div className="grid gap-4 sm:grid-cols-2 lg:grid-cols-3">
            {builtInProviders.map((provider) => (
              <BuiltInProviderCard key={provider.name} provider={provider} />
            ))}
          </div>
        </div>
      )}

      {/* User providers section */}
      <div>
        <h2 className="text-lg font-medium mb-3">My Providers</h2>

        {/* Stats */}
        {!isLoading && providers.length > 0 && (
          <div className="flex items-center gap-4 mb-4">
            <Badge variant="secondary">{enabledCount} enabled</Badge>
            {disabledCount > 0 && <Badge variant="outline">{disabledCount} disabled</Badge>}
          </div>
        )}

        {/* Search */}
        {providers.length > 0 && (
          <div className="mb-4">
            <Input
              placeholder="Search providers..."
              value={search}
              onChange={(e) => setSearch(e.target.value)}
              className="max-w-sm"
            />
          </div>
        )}

        {/* Error state */}
        {error && (
          <div className="rounded-md bg-destructive/10 px-4 py-3 text-sm text-destructive mb-4">
            Failed to load providers. Please try again.
          </div>
        )}

        {/* Loading state */}
        {isLoading && (
          <div className="grid gap-4 sm:grid-cols-2 lg:grid-cols-3">
            {Array.from({ length: 3 }).map((_, i) => (
              <ProviderCardSkeleton key={i} />
            ))}
          </div>
        )}

        {/* Empty state */}
        {!isLoading && providers.length === 0 && (
          <div className="text-center py-12">
            <WifiOff className="h-12 w-12 text-muted-foreground mx-auto mb-4" />
            <h2 className="text-lg font-medium mb-2">No custom providers yet</h2>
            <p className="text-sm text-muted-foreground max-w-md mx-auto mb-4">
              Add your own API keys for OpenAI, Anthropic, and other providers to use custom models
              in chat.
            </p>
            <Button onClick={() => setIsCreateModalOpen(true)}>
              <Plus className="h-4 w-4 mr-2" />
              Add Provider
            </Button>
          </div>
        )}

        {/* Empty search results */}
        {!isLoading && providers.length > 0 && filteredProviders.length === 0 && (
          <div className="text-center py-12">
            <Server className="h-12 w-12 text-muted-foreground mx-auto mb-4" />
            <h2 className="text-lg font-medium mb-2">No matching providers</h2>
            <p className="text-sm text-muted-foreground">
              Try adjusting your search terms or{" "}
              <button onClick={() => setSearch("")} className="text-primary hover:underline">
                clear the search
              </button>
            </p>
          </div>
        )}

        {/* Provider cards grid */}
        {!isLoading && filteredProviders.length > 0 && (
          <div className="grid gap-4 sm:grid-cols-2 lg:grid-cols-3">
            {filteredProviders.map((provider) => (
              <ProviderCard
                key={provider.id}
                provider={provider}
                onEdit={setEditingProvider}
                onDelete={handleDelete}
                onTest={handleTest}
                testResult={testResults[provider.id]}
                isTesting={testingIds.has(provider.id)}
              />
            ))}
          </div>
        )}
      </div>

      {/* Create modal */}
      <SelfServiceProviderFormModal
        isOpen={isCreateModalOpen}
        onClose={() => setIsCreateModalOpen(false)}
        onCreateSubmit={handleCreateSubmit}
        onEditSubmit={() => {}}
        isLoading={createMutation.isPending}
      />

      {/* Edit modal */}
      <SelfServiceProviderFormModal
        isOpen={!!editingProvider}
        onClose={() => setEditingProvider(null)}
        onCreateSubmit={() => {}}
        onEditSubmit={handleEditSubmit}
        isLoading={updateMutation.isPending}
        editingProvider={editingProvider}
      />
    </div>
  );
}
