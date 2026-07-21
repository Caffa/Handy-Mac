import { vi } from 'vitest';

// Mock @tauri-apps/api/event
vi.mock('@tauri-apps/api/event', () => ({
  listen: vi.fn().mockResolvedValue(() => {}),
  emit: vi.fn().mockResolvedValue(undefined),
}));

// Mock @tauri-apps/api/core
vi.mock('@tauri-apps/api/core', () => ({
  invoke: vi.fn().mockResolvedValue(undefined),
  convertFileSrc: vi.fn((path: string) => `http://localhost/${path}`),
}));

// Mock @/bindings — commands that the stores call
vi.mock('@/bindings', () => ({
  commands: {
    getAppSettings: vi.fn().mockResolvedValue({
      push_to_talk: true,
      audio_feedback: false,
      onboarding_completed: false,
      selected_language: 'en',
      overlay_position: 'top',
      overlay_style: 'minimal',
      vad_sensitivity: 'balanced',
      paste_method: 'ctrl_v',
      theme: 'system',
      history_limit: 100,
      overlay_scale: 1.0,
      word_correction_threshold: 0.8,
      auto_submit: false,
      auto_submit_key: 'enter',
      live_captions_enabled: false,
      hybrid_mode_enabled: false,
      hybrid_threshold_secs: 30.0,
      transcribe_accelerator: 'auto',
      ort_accelerator: 'auto',
      transcribe_gpu_device: -1,
      model_unload_timeout: 'min5',
      noise_suppression_level: 'medium',
      sound_theme: 'marimba',
      keyboard_implementation: 'tauri',
      clipboard_handling: 'copy_to_clipboard',
      post_process_providers: [],
      post_process_api_keys: {},
      selected_model: 'large-v3',
      bindings: {},
      custom_words: [],
      advanced_custom_words: [],
      overlay_screen_target: 'cursor',
      recording_retention_period: 'never',
      typing_tool: 'auto',
      spelling_dictionary: 'dwyl',
      log_level: 'info',
    }),
    updateAppSetting: vi.fn().mockResolvedValue(undefined),
    getModels: vi.fn().mockResolvedValue([]),
    selectModel: vi.fn().mockResolvedValue(true),
    downloadModel: vi.fn().mockResolvedValue(undefined),
    deleteModel: vi.fn().mockResolvedValue(undefined),
    getAvailableModels: vi.fn().mockResolvedValue([]),
  },
}));

// Mock window.__TAURI__
Object.defineProperty(window, '__TAURI__', {
  value: {
    invoke: vi.fn().mockResolvedValue(undefined),
    event: {
      listen: vi.fn().mockResolvedValue(() => {}),
      emit: vi.fn().mockResolvedValue(undefined),
    },
  },
  writable: true,
});
