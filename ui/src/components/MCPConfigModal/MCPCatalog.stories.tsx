import type { Meta, StoryObj } from "@storybook/react";

import { MCPCatalog } from "./MCPCatalog";

const meta = {
  title: "Components/MCPCatalog",
  component: MCPCatalog,
  parameters: {
    layout: "padded",
  },
} satisfies Meta<typeof MCPCatalog>;

export default meta;
type Story = StoryObj<typeof meta>;

export const Live: Story = {
  args: {
    onPick: (p) => alert(`Picked:\n${JSON.stringify(p, null, 2)}`),
    onAddManual: () => alert("Add manually"),
    onCancel: () => alert("Cancel"),
  },
  render: (args) => (
    <div className="max-w-2xl space-y-4">
      <p className="text-sm text-muted-foreground">
        Live view of the catalog against <code>registry.modelcontextprotocol.io</code>. Try
        searching for &ldquo;github&rdquo;, &ldquo;slack&rdquo;, or &ldquo;atlassian&rdquo;.
      </p>
      <MCPCatalog {...args} />
    </div>
  ),
};

export const WithFavorites: Story = {
  args: {
    onPick: (p) => alert(`Picked:\n${JSON.stringify(p, null, 2)}`),
    onAddManual: () => alert("Add manually"),
    onCancel: () => alert("Cancel"),
    favorites: [
      { name: "Platter", url: "io.github.hadriangateway/platter" },
      { name: "Atlassian", url: "https://mcp.atlassian.com/v1/mcp" },
      { name: "Notion", url: "https://mcp.notion.com/mcp" },
      { name: "Hugging Face", url: "https://huggingface.co/mcp" },
      { name: "Miro", url: "https://mcp.miro.com/" },
      { name: "Vercel", url: "https://mcp.vercel.com" },
    ],
  },
  render: (args) => (
    <div className="max-w-4xl space-y-4">
      <p className="text-sm text-muted-foreground">
        Catalog seeded with the default gateway-favorited servers. The Platter entry is a registry
        identifier (resolved via <code>registry.modelcontextprotocol.io</code>), the others are
        direct remote URLs.
      </p>
      <MCPCatalog {...args} />
    </div>
  ),
};
