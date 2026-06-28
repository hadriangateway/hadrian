import type { Meta, StoryObj } from "@storybook/react";
import { VideoResultPlayer } from "./VideoResultPlayer";

// A tiny, valid-enough MP4 `ftyp` box; enough to instantiate a <video> in
// Storybook (it won't actually play).
const STUB_MP4 = new Blob(
  [new Uint8Array([0x00, 0x00, 0x00, 0x18, 0x66, 0x74, 0x79, 0x70, 0x6d, 0x70, 0x34, 0x32])],
  { type: "video/mp4" }
);

const meta = {
  title: "Studio/VideoResultPlayer",
  component: VideoResultPlayer,
  parameters: { layout: "centered" },
  decorators: [
    (Story) => (
      <div className="w-96">
        <Story />
      </div>
    ),
  ],
} satisfies Meta<typeof VideoResultPlayer>;

export default meta;
type Story = StoryObj<typeof meta>;

export const Default: Story = {
  args: { blob: STUB_MP4 },
};
