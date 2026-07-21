import {
  getAppSettings,
  invokeCommand,
  waitForAppReady,
} from '../helpers/tauri-commands.js';

describe('Settings Persistence', () => {
  const originalSettings: Record<string, unknown> = {};

  before(async () => {
    await waitForAppReady();
    // Save original settings so we can restore them after tests
    const settings = await getAppSettings();
    Object.assign(originalSettings, settings);
  });

  after(async () => {
    // Restore original theme setting
    if (originalSettings.theme) {
      await invokeCommand('change_theme_setting', {
        theme: originalSettings.theme as string,
      });
    }
  });

  it('should change and persist theme setting', async () => {
    // Change theme
    await invokeCommand('change_theme_setting', { theme: 'dark' });

    // Verify change
    const settings = await getAppSettings();
    expect(settings.theme).toBe('dark');

    // Reload app
    await browser.execute(() => window.location.reload());
    await browser.pause(2000);

    // Verify persistence after reload
    const reloaded = await getAppSettings();
    expect(reloaded.theme).toBe('dark');
  });

  it('should change and persist overlay position', async () => {
    await invokeCommand('change_overlay_position_setting', {
      position: 'top',
    });

    const settings = await getAppSettings();
    expect(settings.overlay_position).toBe('top');
  });

  it('should save multiple settings in sequence', async () => {
    await invokeCommand('change_theme_setting', { theme: 'light' });
    await invokeCommand('change_overlay_position_setting', {
      position: 'bottom',
    });
    await invokeCommand('change_vad_enabled_setting', { vad_enabled: false });

    const settings = await getAppSettings();
    expect(settings.theme).toBe('light');
    expect(settings.overlay_position).toBe('bottom');
    expect(settings.vad_enabled).toBe(false);
  });
});