import type { Meta, StoryObj } from "@storybook/react";
import { Slider } from "./Slider";

const meta: Meta<typeof Slider> = {
  title: "UI/Slider",
  component: Slider,
  tags: ["autodocs"],
  argTypes: {
    value: {
      control: "number",
    },
    min: {
      control: "number",
    },
    max: {
      control: "number",
    },
    step: {
      control: "number",
    },
    disabled: {
      control: "boolean",
    },
    label: {
      control: "text",
    },
    description: {
      control: "text",
    },
    descriptionMode: {
      control: "select",
      options: ["inline", "tooltip"],
    },
    grouped: {
      control: "boolean",
    },
    showValue: {
      control: "boolean",
    },
    onChange: { action: "changed" },
  },
  args: {
    value: 0.5,
    min: 0,
    max: 1,
    step: 0.01,
    label: "Volume",
    description: "Adjust the output volume level",
    showValue: true,
  },
};

export default meta;
type Story = StoryObj<typeof Slider>;

// --- Basic stories ---

export const Default: Story = {
  args: {
    value: 0.5,
  },
};

export const AtMinimum: Story = {
  args: {
    value: 0,
  },
};

export const AtMaximum: Story = {
  args: {
    value: 1,
  },
};

// --- Range stories ---

export const PercentageRange: Story = {
  args: {
    value: 75,
    min: 0,
    max: 100,
    step: 1,
    formatValue: (v: number) => `${Math.round(v)}%`,
    label: "Opacity",
    description: "Set the overlay opacity percentage",
  },
};

export const IntegerRange: Story = {
  args: {
    value: 5,
    min: 0,
    max: 10,
    step: 1,
    formatValue: (v: number) => `${Math.round(v)}/10`,
    label: "Quality",
    description: "Set the quality level from 1 to 10",
  },
};

// --- State stories ---

export const Disabled: Story = {
  args: {
    value: 0.5,
    disabled: true,
  },
};

export const HiddenValue: Story = {
  args: {
    value: 0.75,
    showValue: false,
  },
};

// --- Description mode stories ---

export const InlineDescription: Story = {
  args: {
    descriptionMode: "inline",
    description: "This description appears inline below the label",
  },
};

export const Grouped: Story = {
  args: {
    grouped: true,
  },
};