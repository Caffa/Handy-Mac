const { resolve, dirname } = require("path");

const config = {
  stories: [
    "../src/**/*.mdx",
    "../src/**/*.stories.@(js|jsx|mjs|ts|tsx)",
  ],
  addons: [
    "@storybook/addon-essentials",
    "@storybook/addon-interactions",
  ],
  framework: {
    name: "@storybook/react-vite",
    options: {},
  },
  viteFinal: async (config) => {
    // Add path aliases to match the project's tsconfig and vite config
    config.resolve = config.resolve || {};
    config.resolve.alias = {
      ...config.resolve.alias,
      "@": resolve(__dirname, "../src"),
      "@/bindings": resolve(__dirname, "../src/bindings.ts"),
    };
    return config;
  },
};

module.exports = config;