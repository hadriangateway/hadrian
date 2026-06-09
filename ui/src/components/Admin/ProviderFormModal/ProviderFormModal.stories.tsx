import type { Meta, StoryObj } from "@storybook/react";
import { http, HttpResponse } from "msw";

import { ProviderFormModal } from "./ProviderFormModal";

const meta: Meta<typeof ProviderFormModal> = {
  title: "Admin/ProviderFormModal",
  component: ProviderFormModal,
  parameters: {
    layout: "centered",
    msw: {
      handlers: [
        http.post("*/admin/v1/dynamic-providers/test-credentials", () => {
          return HttpResponse.json({
            status: "ok",
            message: "Connected successfully. 8 models available.",
            latency_ms: 230,
          });
        }),
      ],
    },
  },
};

export default meta;
type Story = StoryObj<typeof ProviderFormModal>;

const mockOrganizations = [
  {
    id: "org_1",
    slug: "acme-corp",
    name: "Acme Corporation",
    created_at: "2024-01-01T00:00:00Z",
    updated_at: "2024-01-01T00:00:00Z",
  },
  {
    id: "org_2",
    slug: "startup-inc",
    name: "Startup Inc",
    created_at: "2024-01-02T00:00:00Z",
    updated_at: "2024-01-02T00:00:00Z",
  },
];

const mockProvider = {
  id: "prov_1",
  name: "my-openai",
  provider_type: "open_ai",
  base_url: "https://api.openai.com/v1",
  has_api_key: true,
  config: null,
  models: ["gpt-4", "gpt-3.5-turbo"],
  is_enabled: true,
  owner: { type: "organization" as const, org_id: "org_1" },
  created_at: "2024-01-01T00:00:00Z",
  updated_at: "2024-01-01T00:00:00Z",
};

const mockBedrockProvider = {
  id: "prov_2",
  name: "my-bedrock",
  provider_type: "bedrock",
  base_url: "",
  has_api_key: false,
  config: {
    region: "us-east-1",
    credentials: { type: "default" },
  },
  models: ["anthropic.claude-3-5-sonnet-20241022-v2:0"],
  is_enabled: true,
  owner: { type: "organization" as const, org_id: "org_1" },
  created_at: "2024-01-01T00:00:00Z",
  updated_at: "2024-01-01T00:00:00Z",
};

const mockVertexProvider = {
  id: "prov_3",
  name: "my-vertex",
  provider_type: "vertex",
  base_url: "",
  has_api_key: false,
  config: {
    project: "acme-ml-prod",
    region: "us-central1",
    credentials: { type: "default" },
  },
  models: ["gemini-2.0-flash"],
  is_enabled: true,
  owner: { type: "organization" as const, org_id: "org_1" },
  created_at: "2024-01-01T00:00:00Z",
  updated_at: "2024-01-01T00:00:00Z",
};

const mockGeminiProvider = {
  id: "prov_4",
  name: "my-gemini",
  provider_type: "gemini",
  base_url: "",
  has_api_key: true,
  config: {},
  models: ["gemini-2.0-flash", "gemini-2.5-pro"],
  is_enabled: true,
  owner: { type: "organization" as const, org_id: "org_1" },
  created_at: "2024-01-01T00:00:00Z",
  updated_at: "2024-01-01T00:00:00Z",
};

export const CreateMode: Story = {
  args: {
    isOpen: true,
    onClose: () => console.log("Close"),
    onCreateSubmit: (data) => console.log("Create", data),
    onEditSubmit: (data) => console.log("Edit", data),
    organizations: mockOrganizations,
  },
};

export const EditMode: Story = {
  args: {
    isOpen: true,
    onClose: () => console.log("Close"),
    onCreateSubmit: (data) => console.log("Create", data),
    onEditSubmit: (data) => console.log("Edit", data),
    editingProvider: mockProvider,
    organizations: mockOrganizations,
  },
};

export const Loading: Story = {
  args: {
    isOpen: true,
    onClose: () => console.log("Close"),
    onCreateSubmit: (data) => console.log("Create", data),
    onEditSubmit: (data) => console.log("Edit", data),
    organizations: mockOrganizations,
    isLoading: true,
  },
};

export const EditModeDisabled: Story = {
  args: {
    isOpen: true,
    onClose: () => console.log("Close"),
    onCreateSubmit: (data) => console.log("Create", data),
    onEditSubmit: (data) => console.log("Edit", data),
    editingProvider: { ...mockProvider, is_enabled: false },
    organizations: mockOrganizations,
  },
};

export const NoOrganizations: Story = {
  args: {
    isOpen: true,
    onClose: () => console.log("Close"),
    onCreateSubmit: (data) => console.log("Create", data),
    onEditSubmit: (data) => console.log("Edit", data),
    organizations: [],
  },
};

export const EditBedrock: Story = {
  args: {
    isOpen: true,
    onClose: () => console.log("Close"),
    onCreateSubmit: (data) => console.log("Create", data),
    onEditSubmit: (data) => console.log("Edit", data),
    editingProvider: mockBedrockProvider,
    organizations: mockOrganizations,
  },
};

export const EditVertex: Story = {
  args: {
    isOpen: true,
    onClose: () => console.log("Close"),
    onCreateSubmit: (data) => console.log("Create", data),
    onEditSubmit: (data) => console.log("Edit", data),
    editingProvider: mockVertexProvider,
    organizations: mockOrganizations,
  },
};

export const EditGemini: Story = {
  args: {
    isOpen: true,
    onClose: () => console.log("Close"),
    onCreateSubmit: (data) => console.log("Create", data),
    onEditSubmit: (data) => console.log("Edit", data),
    editingProvider: mockGeminiProvider,
    organizations: mockOrganizations,
  },
};
