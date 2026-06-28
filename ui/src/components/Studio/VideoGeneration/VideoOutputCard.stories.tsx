import type { Meta, StoryObj } from "@storybook/react";
import type { VideoHistoryEntry } from "@/pages/studio/types";
import { VideoOutputCard } from "./VideoOutputCard";

const ENTRY: VideoHistoryEntry = {
  id: "entry-1",
  jobId: "video_abc123",
  prompt: "A cat surfing a wave at sunset, cinematic",
  modelId: "sora-2",
  status: "completed",
  options: { seconds: "8", size: "1280x720" },
  // No OPFS blob in Storybook → the card renders its "unavailable" fallback.
  videoData: "",
  costMicrocents: 800_000,
  createdAt: Date.now(),
};

const meta = {
  title: "Studio/VideoOutputCard",
  component: VideoOutputCard,
  parameters: { layout: "centered" },
  decorators: [
    (Story) => (
      <div className="w-80">
        <Story />
      </div>
    ),
  ],
} satisfies Meta<typeof VideoOutputCard>;

export default meta;
type Story = StoryObj<typeof meta>;

export const Default: Story = {
  args: { entry: ENTRY, onRemove: () => {} },
};
