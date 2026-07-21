import { waitForAppReady } from '../helpers/tauri-commands.js';

describe('App Lifecycle', () => {
  before(async () => {
    await waitForAppReady();
  });

  it('should start the app without crashing', async () => {
    const title = await browser.getTitle();
    expect(title).toBeTruthy();
  });

  it('should have a main window', async () => {
    const windows = await browser.getWindowHandles();
    expect(windows.length).toBeGreaterThanOrEqual(1);
  });

  it('should respond to get_app_settings command', async () => {
    const settings = await browser.tauri.execute(({ core }) => {
      return core.invoke('get_app_settings');
    });
    expect(settings).toBeTruthy();
    expect(settings.theme).toBeDefined();
  });

  it('should have default settings values', async () => {
    const settings = await browser.tauri.execute(({ core }) => {
      return core.invoke('get_app_settings');
    });
    expect(settings.language).toBe('en');
    expect(settings.overlay_style).toBeDefined();
  });
});