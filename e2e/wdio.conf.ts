import type { Options } from '@wdio/types';

export const config: Options.Testrunner = {
  runner: 'local',
  specs: ['./e2e/specs/**/*.spec.ts'],
  maxInstances: 1, // Only one instance for desktop app testing

  services: ['@wdio/tauri-service'],

  capabilities: [
    {
      browserName: 'tauri',
      'tauri:options': {
        application: './src-tauri/target/release/Handy-Mac',
      },
    },
  ],

  framework: 'mocha',
  mochaOpts: {
    ui: 'bdd',
    timeout: 60000, // 60s timeout for desktop tests
  },

  reporters: ['spec'],

  // Hooks for setup/teardown
  onPrepare: () => {
    /* Called before tests */
  },
  onComplete: () => {
    /* Called after all tests */
  },
};