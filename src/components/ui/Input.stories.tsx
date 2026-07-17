import type { Meta, StoryObj } from "@storybook/react-vite";
import { Input } from "./Input";

const meta: Meta<typeof Input> = {
  title: "UI/Input",
  component: Input,
  tags: ["autodocs"],
  argTypes: {
    variant: {
      control: "select",
      options: ["default", "compact"],
    },
    disabled: {
      control: "boolean",
    },
    placeholder: {
      control: "text",
    },
    value: {
      control: "text",
    },
    type: {
      control: "select",
      options: ["text", "password", "email", "number", "search", "url"],
    },
  },
  args: {
    placeholder: "Enter text...",
    variant: "default",
  },
};

export default meta;
type Story = StoryObj<typeof Input>;

// --- Variant stories ---

export const Default: Story = {
  args: {
    variant: "default",
    placeholder: "Default input",
  },
};

export const Compact: Story = {
  args: {
    variant: "compact",
    placeholder: "Compact input",
  },
};

// --- State stories ---

export const WithValue: Story = {
  args: {
    value: "Hello, world",
    placeholder: "Enter text...",
  },
};

export const Disabled: Story = {
  args: {
    disabled: true,
    placeholder: "Disabled input",
  },
};

export const DisabledWithValue: Story = {
  args: {
    disabled: true,
    value: "Cannot edit this",
  },
};

// --- Type variations ---

export const PasswordInput: Story = {
  args: {
    type: "password",
    placeholder: "Enter password...",
  },
};

export const EmailInput: Story = {
  args: {
    type: "email",
    placeholder: "user@example.com",
  },
};

export const NumberInput: Story = {
  args: {
    type: "number",
    placeholder: "0",
    min: 0,
    max: 100,
  },
};

export const SearchInput: Story = {
  args: {
    type: "search",
    placeholder: "Search...",
    variant: "default",
  },
};