/**
 * Helper functions for invoking Tauri commands in E2E tests.
 * Uses the @wdio/tauri-plugin's browser.tauri.execute() API
 * for clean integration with Tauri v2 commands.
 */

/**
 * Invoke a Tauri command by name with optional arguments.
 * Uses browser.tauri.execute() which provides access to the Tauri core API
 * including invoke(), window management, and event listeners.
 */
export async function invokeCommand<T>(
  command: string,
  args?: Record<string, unknown>,
): Promise<T> {
  return browser.tauri.execute(({ core }, cmd, cmdArgs) => {
    return core.invoke(cmd, cmdArgs) as Promise<T>;
  }, command, args ?? {});
}

/**
 * Get the full app settings object from the Tauri backend.
 */
export async function getAppSettings(): Promise<Record<string, unknown>> {
  return invokeCommand('get_app_settings');
}

/**
 * Change a named setting by invoking its dedicated Tauri command.
 * Most settings commands accept a single value argument.
 */
export async function changeSetting(
  command: string,
  value: unknown,
): Promise<void> {
  return invokeCommand(command, { value });
}

/**
 * Check if the app is currently recording audio.
 */
export async function isRecording(): Promise<boolean> {
  return invokeCommand('is_recording');
}

/**
 * Cancel the current recording or processing operation.
 */
export async function cancelOperation(): Promise<void> {
  return invokeCommand('cancel_operation');
}

/**
 * Wait until the app is ready (settings command responds successfully).
 * Useful in before() hooks to ensure the app has finished loading.
 */
export async function waitForAppReady(timeout = 10000): Promise<void> {
  await browser.waitUntil(
    async () => {
      try {
        const settings = await getAppSettings();
        return settings !== null && settings !== undefined;
      } catch {
        return false;
      }
    },
    { timeout, timeoutMsg: 'App did not become ready in time' },
  );
}