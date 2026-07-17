import type { Meta, StoryObj } from "@storybook/react";
import { ToggleSwitch } from "./ToggleSwitch";

const meta: Meta<typeof ToggleSwitch> = {
  title: "UI/ToggleSwitch",
  component: ToggleSwitch,
  tags: ["autodocs"],
  argTypes: {
    checked: {
      control: "boolean",
    },
    disabled: {
      control: "boolean",
    },
    isUpdating: {
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
    tooltipPosition: {
      control: "select",
      options: ["top", "bottom"],
    },
    onChange: { action: "changed" },
  },
  args: {
    checked: false,
    label: "Enable feature",
    description: "Turn this feature on or off",
    descriptionMode: "tooltip",
  },
};

export default meta;
type Story = StoryObj<typeof ToggleSwitch>;

// --- State stories ---

export const Unchecked: Story = {
  args: {
    checked: false,
  },
};

export const Checked: Story = {
  args: {
    checked: true,
  },
};

export const Disabled: Story = {
  args: {
    checked: false,
    disabled: true,
  },
};

export const DisabledChecked: Story = {
  args: {
    checked: true,
    disabled: true,
  },
};

// --- Description mode stories ---

export const InlineDescription: Story = {
  args: {
    descriptionMode: "inline",
    description: "This description appears inline below the label",
  },
};

export const TooltipDescription: Story = {
  args: {
    descriptionMode: "tooltip",
    description: "Hover the info icon to see this description",
  },
};

// --- Updating state ---

export const Updating: Story = {
  args: {
    isUpdating: true,
  },
};

// --- Grouped variant ---

export const Grouped: Story = {
  args: {
    grouped: true,
    description: "This toggle is part of a grouped setting",
  },
};