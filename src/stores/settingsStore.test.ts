import { describe, it, expect, vi, beforeEach } from 'vitest';
import { useSettingsStore } from './settingsStore';

// Reset the store between tests
beforeEach(() => {
  useSettingsStore.setState({
    settings: null,
    defaultSettings: null,
    isLoading: false,
    isUpdating: {},
    audioDevices: [],
    outputDevices: [],
    customSounds: { start: false, stop: false },
    postProcessModelOptions: {},
  });
  vi.clearAllMocks();
});

describe('settingsStore', () => {
  describe('initial state', () => {
    it('starts with null settings', () => {
      const state = useSettingsStore.getState();
      expect(state.settings).toBeNull();
      expect(state.defaultSettings).toBeNull();
    });

    it('starts with empty audio devices', () => {
      const state = useSettingsStore.getState();
      expect(state.audioDevices).toEqual([]);
      expect(state.outputDevices).toEqual([]);
    });

    it('starts with loading false', () => {
      const state = useSettingsStore.getState();
      expect(state.isLoading).toBe(false);
    });

    it('starts with empty isUpdating map', () => {
      const state = useSettingsStore.getState();
      expect(state.isUpdating).toEqual({});
    });
  });

  describe('setSettings', () => {
    it('updates settings state', () => {
      const mockSettings = {
        push_to_talk: true,
        audio_feedback: false,
        theme: 'dark',
      } as any;

      useSettingsStore.getState().setSettings(mockSettings);

      expect(useSettingsStore.getState().settings).toEqual(mockSettings);
    });

    it('can set settings to null', () => {
      useSettingsStore.getState().setSettings({ push_to_talk: true } as any);
      useSettingsStore.getState().setSettings(null);

      expect(useSettingsStore.getState().settings).toBeNull();
    });
  });

  describe('setLoading', () => {
    it('sets loading state to true', () => {
      useSettingsStore.getState().setLoading(true);
      expect(useSettingsStore.getState().isLoading).toBe(true);
    });

    it('sets loading state to false', () => {
      useSettingsStore.getState().setLoading(true);
      useSettingsStore.getState().setLoading(false);
      expect(useSettingsStore.getState().isLoading).toBe(false);
    });
  });

  describe('setUpdating', () => {
    it('tracks updating state for a key', () => {
      useSettingsStore.getState().setUpdating('push_to_talk', true);
      expect(useSettingsStore.getState().isUpdating['push_to_talk']).toBe(true);
    });

    it('clears updating state for a key', () => {
      useSettingsStore.getState().setUpdating('push_to_talk', true);
      useSettingsStore.getState().setUpdating('push_to_talk', false);
      expect(useSettingsStore.getState().isUpdating['push_to_talk']).toBe(false);
    });

    it('tracks multiple keys independently', () => {
      useSettingsStore.getState().setUpdating('push_to_talk', true);
      useSettingsStore.getState().setUpdating('theme', true);

      const state = useSettingsStore.getState();
      expect(state.isUpdating['push_to_talk']).toBe(true);
      expect(state.isUpdating['theme']).toBe(true);
    });
  });

  describe('setAudioDevices', () => {
    it('updates audio devices list', () => {
      const devices = [
        { index: 0, name: 'Mic 1', is_default: true },
        { index: 1, name: 'Mic 2', is_default: false },
      ];

      useSettingsStore.getState().setAudioDevices(devices as any);
      expect(useSettingsStore.getState().audioDevices).toEqual(devices);
    });
  });

  describe('setOutputDevices', () => {
    it('updates output devices list', () => {
      const devices = [
        { index: 0, name: 'Speaker 1', is_default: true },
      ];

      useSettingsStore.getState().setOutputDevices(devices as any);
      expect(useSettingsStore.getState().outputDevices).toEqual(devices);
    });
  });

  describe('setCustomSounds', () => {
    it('updates custom sounds state', () => {
      useSettingsStore.getState().setCustomSounds({ start: true, stop: false });
      expect(useSettingsStore.getState().customSounds).toEqual({
        start: true,
        stop: false,
      });
    });
  });

  describe('setPostProcessModelOptions', () => {
    it('stores model options for a provider', () => {
      useSettingsStore
        .getState()
        .setPostProcessModelOptions('openai', ['gpt-4', 'gpt-3.5']);

      expect(
        useSettingsStore.getState().postProcessModelOptions['openai']
      ).toEqual(['gpt-4', 'gpt-3.5']);
    });

    it('overwrites existing options for same provider', () => {
      useSettingsStore
        .getState()
        .setPostProcessModelOptions('openai', ['gpt-4']);
      useSettingsStore
        .getState()
        .setPostProcessModelOptions('openai', ['gpt-4', 'gpt-4-turbo']);

      expect(
        useSettingsStore.getState().postProcessModelOptions['openai']
      ).toEqual(['gpt-4', 'gpt-4-turbo']);
    });

    it('stores options for multiple providers independently', () => {
      useSettingsStore
        .getState()
        .setPostProcessModelOptions('openai', ['gpt-4']);
      useSettingsStore
        .getState()
        .setPostProcessModelOptions('anthropic', ['claude-3']);

      const state = useSettingsStore.getState();
      expect(state.postProcessModelOptions['openai']).toEqual(['gpt-4']);
      expect(state.postProcessModelOptions['anthropic']).toEqual(['claude-3']);
    });
  });

  describe('getSetting', () => {
    it('returns undefined when settings is null', () => {
      const value = useSettingsStore.getState().getSetting('push_to_talk');
      expect(value).toBeUndefined();
    });

    it('returns setting value when settings exists', () => {
      useSettingsStore.getState().setSettings({
        push_to_talk: true,
        theme: 'dark',
      } as any);

      expect(useSettingsStore.getState().getSetting('push_to_talk')).toBe(true);
      expect(useSettingsStore.getState().getSetting('theme')).toBe('dark');
    });
  });

  describe('isUpdatingKey', () => {
    it('returns false for unknown keys', () => {
      expect(useSettingsStore.getState().isUpdatingKey('unknown_key')).toBe(
        false
      );
    });

    it('returns true for keys being updated', () => {
      useSettingsStore.getState().setUpdating('push_to_talk', true);
      expect(useSettingsStore.getState().isUpdatingKey('push_to_talk')).toBe(
        true
      );
    });

    it('returns false for keys no longer updating', () => {
      useSettingsStore.getState().setUpdating('push_to_talk', true);
      useSettingsStore.getState().setUpdating('push_to_talk', false);
      expect(useSettingsStore.getState().isUpdatingKey('push_to_talk')).toBe(
        false
      );
    });
  });

  describe('theme changes', () => {
    it('handles light/dark/system theme values', () => {
      for (const theme of ['light', 'dark', 'system']) {
        useSettingsStore.getState().setSettings({ theme } as any);
        expect(useSettingsStore.getState().settings?.theme).toBe(theme);
      }
    });
  });

  describe('overlay style changes', () => {
    it('handles none/minimal/live overlay styles', () => {
      for (const style of ['none', 'minimal', 'live']) {
        useSettingsStore.getState().setSettings({ overlay_style: style } as any);
        expect(useSettingsStore.getState().settings?.overlay_style).toBe(style);
      }
    });
  });

  describe('VAD sensitivity changes', () => {
    it('handles all 5 sensitivity levels', () => {
      const levels = [
        'very_quick',
        'quick',
        'balanced',
        'relaxed',
        'very_relaxed',
      ];
      for (const level of levels) {
        useSettingsStore
          .getState()
          .setSettings({ vad_sensitivity: level } as any);
        expect(useSettingsStore.getState().settings?.vad_sensitivity).toBe(
          level
        );
      }
    });
  });
});
