import type { Meta, StoryObj } from "@storybook/react";
import { VisualizerBars } from "./VisualizerBars";

const meta: Meta<typeof VisualizerBars> = {
  title: "Overlay/VisualizerBars",
  component: VisualizerBars,
  tags: ["autodocs"],
  argTypes: {
    levels: {
      control: "object",
      description: "Array of audio level values (0 to 1)",
    },
    isRouter: {
      control: "boolean",
      description: "Whether the overlay is in router mode (changes bar color)",
    },
  },
  args: {
    levels: [0.3, 0.5, 0.7, 0.5, 0.3],
    isRouter: false,
  },
  decorators: [
    (Story) => (
      <div
        style={{
          background: "#000000cc",
          borderRadius: "30px",
          padding: "8px 16px",
          display: "inline-flex",
          alignItems: "center",
        }}
      >
        <Story />
      </div>
    ),
  ],
};

export default meta;
type Story = StoryObj<typeof VisualizerBars>;

// --- Level stories ---

export const EmptyLevels: Story = {
  args: {
    levels: [],
    isRouter: false,
  },
};

export const FewBars: Story = {
  args: {
    levels: [0.3, 0.6, 0.4],
    isRouter: false,
  },
};

export const ManyBars: Story = {
  args: {
    levels: [0.2, 0.4, 0.6, 0.8, 0.9, 0.7, 0.5, 0.3, 0.1, 0.2],
    isRouter: false,
  },
};

export const AllMaxBars: Story = {
  args: {
    levels: [1, 1, 1, 1, 1, 1, 1],
    isRouter: false,
  },
};

export const QuietBars: Story = {
  args: {
    levels: [0.05, 0.1, 0.08, 0.05, 0.12],
    isRouter: false,
  },
};

// --- Router mode stories ---

export const RouterMode: Story = {
  args: {
    levels: [0.3, 0.5, 0.7, 0.5, 0.3],
    isRouter: true,
  },
};

export const RouterModeManyBars: Story = {
  args: {
    levels: [0.2, 0.4, 0.6, 0.8, 0.9, 0.7, 0.5, 0.3, 0.1, 0.2],
    isRouter: true,
  },
};

export const RouterModeAllMax: Story = {
  args: {
    levels: [1, 1, 1, 1, 1, 1, 1],
    isRouter: true,
  },
};