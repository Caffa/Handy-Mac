import type { Meta, StoryObj } from "@storybook/react-vite";
import Badge from "./Badge";

const meta: Meta<typeof Badge> = {
  title: "UI/Badge",
  component: Badge,
  tags: ["autodocs"],
  argTypes: {
    variant: {
      control: "select",
      options: ["primary", "success", "secondary"],
    },
    className: {
      control: "text",
    },
    title: {
      control: "text",
    },
  },
  args: {
    children: "Badge text",
    variant: "primary",
  },
};

export default meta;
type Story = StoryObj<typeof Badge>;

// --- Variant stories ---

export const Primary: Story = {
  args: {
    variant: "primary",
  },
};

export const Success: Story = {
  args: {
    variant: "success",
  },
};

export const Secondary: Story = {
  args: {
    variant: "secondary",
  },
};

// --- With title ---

export const WithTitle: Story = {
  args: {
    variant: "primary",
    title: "This is a tooltip title",
  },
};

// --- With custom className ---

export const CustomClassName: Story = {
  args: {
    variant: "success",
    className: "uppercase tracking-wider",
  },
};

// --- Content variations ---

export const NumberBadge: Story = {
  args: {
    children: "42",
    variant: "primary",
  },
};

export const StatusBadge: Story = {
  args: {
    children: "Active",
    variant: "success",
  },
};

export const InfoBadge: Story = {
  args: {
    children: "Draft",
    variant: "secondary",
  },
};