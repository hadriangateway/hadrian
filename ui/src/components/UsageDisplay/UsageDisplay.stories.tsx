import type { Meta, StoryObj } from "@storybook/react";

import { UsageDisplay } from "./UsageDisplay";

const meta: Meta<typeof UsageDisplay> = {
  title: "Chat/UsageDisplay",
  component: UsageDisplay,
  parameters: {
    layout: "centered",
  },
};

export default meta;
type Story = StoryObj<typeof meta>;

export const Default: Story = {
  args: {
    usage: {
      inputTokens: 150,
      outputTokens: 350,
      totalTokens: 500,
      cost: 0.0025,
    },
  },
};

export const HighUsage: Story = {
  args: {
    usage: {
      inputTokens: 15000,
      outputTokens: 8500,
      totalTokens: 23500,
      cost: 0.145,
    },
  },
};

export const WithCachedTokens: Story = {
  args: {
    usage: {
      inputTokens: 1200,
      outputTokens: 800,
      totalTokens: 2000,
      cost: 0.012,
      cachedTokens: 500,
    },
  },
};

export const WithReasoningTokens: Story = {
  args: {
    usage: {
      inputTokens: 500,
      outputTokens: 2500,
      totalTokens: 3000,
      cost: 0.085,
      reasoningTokens: 1200,
    },
  },
};

export const AllDetails: Story = {
  args: {
    usage: {
      inputTokens: 2500,
      outputTokens: 4500,
      totalTokens: 7000,
      cost: 0.0875,
      cachedTokens: 800,
      reasoningTokens: 1500,
    },
  },
};

/**
 * Running mid-stream total during a server-tool loop: the whole label
 * pulses in a slightly brighter color, tooltip reframed as
 * "running total" / "cost so far".
 */
export const Provisional: Story = {
  args: {
    usage: {
      inputTokens: 300,
      outputTokens: 80,
      totalTokens: 380,
      cost: 0.003,
    },
    provisional: true,
  },
};

export const Compact: Story = {
  args: {
    usage: {
      inputTokens: 150,
      outputTokens: 350,
      totalTokens: 500,
      cost: 0.0025,
    },
    compact: true,
  },
};

export const NoCost: Story = {
  args: {
    usage: {
      inputTokens: 100,
      outputTokens: 200,
      totalTokens: 300,
    },
  },
};

export const ZeroCost: Story = {
  args: {
    usage: {
      inputTokens: 100,
      outputTokens: 200,
      totalTokens: 300,
      cost: 0,
    },
  },
};

export const WithTimingStats: Story = {
  args: {
    usage: {
      inputTokens: 500,
      outputTokens: 1200,
      totalTokens: 1700,
      cost: 0.025,
      firstTokenMs: 245,
      totalDurationMs: 3500,
      tokensPerSecond: 34.3,
    },
  },
};

export const WithMetaInfo: Story = {
  args: {
    usage: {
      inputTokens: 800,
      outputTokens: 1500,
      totalTokens: 2300,
      cost: 0.035,
      finishReason: "stop",
      modelId: "openai/gpt-4o",
      provider: "openai",
    },
  },
};

export const FullStats: Story = {
  args: {
    usage: {
      inputTokens: 2500,
      outputTokens: 4500,
      totalTokens: 7000,
      cost: 0.0875,
      cachedTokens: 800,
      reasoningTokens: 1500,
      // Timing stats
      firstTokenMs: 180,
      totalDurationMs: 8200,
      tokensPerSecond: 54.9,
      // Meta info
      finishReason: "stop",
      modelId: "anthropic/claude-sonnet-4-20250514",
      provider: "anthropic",
    },
  },
};

export const FastResponse: Story = {
  args: {
    usage: {
      inputTokens: 100,
      outputTokens: 50,
      totalTokens: 150,
      firstTokenMs: 85,
      totalDurationMs: 420,
      tokensPerSecond: 119,
      finishReason: "stop",
      modelId: "groq/llama-3.3-70b-versatile",
      provider: "groq",
    },
  },
};

export const LengthLimited: Story = {
  args: {
    usage: {
      inputTokens: 1000,
      outputTokens: 4096,
      totalTokens: 5096,
      cost: 0.05,
      totalDurationMs: 45000,
      tokensPerSecond: 91,
      finishReason: "length",
      modelId: "openai/gpt-4",
      provider: "openai",
    },
  },
};
