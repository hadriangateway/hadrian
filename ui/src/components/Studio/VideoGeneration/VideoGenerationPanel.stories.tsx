import type { Meta, StoryObj } from "@storybook/react";
import { ToastProvider } from "@/components/Toast/Toast";
import type { ModelInfo } from "@/components/ModelPicker/model-utils";
import { VideoGenerationPanel } from "./VideoGenerationPanel";

const VIDEO_MODELS: ModelInfo[] = [
  { id: "sora-2", tasks: ["video_generation"] } as ModelInfo,
  { id: "sora-2-pro", tasks: ["video_generation"] } as ModelInfo,
];

const meta = {
  title: "Studio/VideoGenerationPanel",
  component: VideoGenerationPanel,
  parameters: {
    layout: "fullscreen",
  },
  decorators: [
    (Story) => (
      <ToastProvider>
        <div className="h-[700px]">
          <Story />
        </div>
      </ToastProvider>
    ),
  ],
} satisfies Meta<typeof VideoGenerationPanel>;

export default meta;
type Story = StoryObj<typeof meta>;

export const Default: Story = {
  args: { availableModels: VIDEO_MODELS },
};

export const NoModels: Story = {
  args: { availableModels: [] },
};
