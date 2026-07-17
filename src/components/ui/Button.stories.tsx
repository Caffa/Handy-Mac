import type { Meta, StoryObj } from "@storybook/react";
import { Button } from "./Button";

const meta: Meta<typeof Button> = {
  title: "UI/Button",
  component: Button,
  tags: ["autodocs"],
  argTypes: {
    variant: {
      control: "select",
      options: [
        "primary",
        "primary-soft",
        "secondary",
        "danger",
        "danger-ghost",
        "ghost",
      ],
    },
    size: {
      control: "select",
      options: ["sm", "md", "lg"],
    },
    disabled: {
      control: "boolean",
    },
    onClick: { action: "clicked" },
  },
  args: {
    children: "Click me",
    variant: "primary",
    size: "md",
  },
};

export default meta;
type Story = StoryObj<typeof Button>;

// --- Variant stories ---

export const Primary: Story = {
  args: {
    variant: "primary",
  },
};

export const PrimarySoft: Story = {
  args: {
    variant: "primary-soft",
  },
};

export const Secondary: Story = {
  args: {
    variant: "secondary",
  },
};

export const Danger: Story = {
  args: {
    variant: "danger",
  },
};

export const DangerGhost: Story = {
  args: {
    variant: "danger-ghost",
  },
};

export const Ghost: Story = {
  args: {
    variant: "ghost",
  },
};

// --- Size stories ---

export const Small: Story = {
  args: {
    size: "sm",
  },
};

export const Medium: Story = {
  args: {
    size: "md",
  },
};

export const Large: Story = {
  args: {
    size: "lg",
  },
};

// --- State stories ---

export const Disabled: Story = {
  args: {
    disabled: true,
  },
};

export const DisabledDanger: Story = {
  args: {
    variant: "danger",
    disabled: true,
  },
};