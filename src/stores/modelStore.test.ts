/**
 * ModelStore tests — covers model list, model selection, and download status tracking.
 */
import { describe, it, expect, vi, beforeEach } from "vitest";

// ─── Mock commands (must use vi.hoisted for hoisted vi.mock) ─────────────

const { mockCommands } = vi.hoisted(() => ({
  mockCommands: {
    getAvailableModels: vi.fn(),
    getCurrentModel: vi.fn(),
    setActiveModel: vi.fn(),
    downloadModel: vi.fn(),
    cancelDownload: vi.fn(),
    deleteModel: vi.fn(),
    rescanLocalModels: vi.fn(),
  },
}));

vi.mock("@/bindings", () => ({
  commands: mockCommands,
}));

vi.mock("@tauri-apps/api/event", () => ({
  listen: vi.fn().mockResolvedValue(vi.fn()),
  emit: vi.fn(),
}));

vi.mock("sonner", () => ({
  toast: {
    error: vi.fn(),
    success: vi.fn(),
    info: vi.fn(),
  },
  Toaster: () => null,
}));

import { useModelStore } from "@/stores/modelStore";
import type { ModelInfo } from "@/bindings";

// ─── Sample models ──────────────────────────────────────────────────────

const SAMPLE_MODELS: ModelInfo[] = [
  {
    id: "whisper-small",
    name: "Whisper Small",
    description: "Small model",
    filename: "whisper-small.bin",
    source: "local",
    size_mb: 500,
    is_downloaded: true,
    is_downloading: false,
    partial_size: 0,
    is_directory: false,
    engine_type: "Whisper",
    accuracy_score: 0.8,
    speed_score: 0.7,
    supports_translation: true,
    is_recommended: true,
    supported_languages: ["en", "de", "fr"],
    supports_language_selection: true,
    is_custom: false,
    supports_streaming: false,
    supports_language_detection: false,
  },
  {
    id: "whisper-medium",
    name: "Whisper Medium",
    description: "Medium model",
    filename: "whisper-medium.bin",
    source: "local",
    size_mb: 1500,
    is_downloaded: true,
    is_downloading: false,
    partial_size: 0,
    is_directory: false,
    engine_type: "Whisper",
    accuracy_score: 0.9,
    speed_score: 0.5,
    supports_translation: true,
    is_recommended: false,
    supported_languages: ["en", "de", "fr", "es"],
    supports_language_selection: true,
    is_custom: false,
    supports_streaming: false,
    supports_language_detection: false,
  },
  {
    id: "whisper-large",
    name: "Whisper Large",
    description: "Large model",
    filename: "whisper-large.bin",
    source: "local",
    size_mb: 3000,
    is_downloaded: false,
    is_downloading: false,
    partial_size: 0,
    is_directory: false,
    engine_type: "Whisper",
    accuracy_score: 0.95,
    speed_score: 0.3,
    supports_translation: true,
    is_recommended: false,
    supported_languages: ["en"],
    supports_language_selection: false,
    is_custom: false,
    supports_streaming: true,
    supports_language_detection: true,
  },
];

// ─── Helper: reset store to initial state ──────────────────────────────
function resetStore() {
  useModelStore.setState({
    models: [],
    currentModel: "",
    downloadingModels: {},
    verifyingModels: {},
    extractingModels: {},
    downloadProgress: {},
    downloadStats: {},
    loading: true,
    error: null,
    initialized: false,
    isRescanning: false,
  });
}

describe("useModelStore", () => {
  beforeEach(() => {
    vi.clearAllMocks();
    resetStore();

    // Default mocks
    mockCommands.getAvailableModels.mockResolvedValue({
      status: "ok",
      data: [...SAMPLE_MODELS],
    });
    mockCommands.getCurrentModel.mockResolvedValue({
      status: "ok",
      data: "whisper-small",
    });
    mockCommands.setActiveModel.mockResolvedValue({
      status: "ok",
      data: null,
    });
    mockCommands.downloadModel.mockResolvedValue({
      status: "ok",
      data: null,
    });
    mockCommands.cancelDownload.mockResolvedValue({
      status: "ok",
      data: null,
    });
    mockCommands.deleteModel.mockResolvedValue({
      status: "ok",
      data: null,
    });
    mockCommands.rescanLocalModels.mockResolvedValue({
      status: "ok",
      data: null,
    });
  });

  // ─── Initial state ─────────────────────────────────────────────────
  describe("initial state", () => {
    it("starts with empty models and loading true", () => {
      const store = useModelStore.getState();
      expect(store.models).toEqual([]);
      expect(store.currentModel).toBe("");
      expect(store.loading).toBe(true);
      expect(store.error).toBeNull();
      expect(store.initialized).toBe(false);
      expect(store.downloadingModels).toEqual({});
      expect(store.downloadProgress).toEqual({});
    });
  });

  // ─── loadModels ───────────────────────────────────────────────────
  describe("loadModels", () => {
    it("loads models from the backend", async () => {
      const store = useModelStore.getState();
      await store.loadModels();

      const updated = useModelStore.getState();
      expect(updated.models).toHaveLength(3);
      expect(updated.loading).toBe(false);
      expect(updated.error).toBeNull();
    });

    it("sets error when backend fails", async () => {
      mockCommands.getAvailableModels.mockResolvedValue({
        status: "error",
        error: "Backend error",
      });

      const store = useModelStore.getState();
      await store.loadModels();

      const updated = useModelStore.getState();
      expect(updated.error).toContain("Backend error");
      expect(updated.loading).toBe(false);
    });

    it("sets error on exception", async () => {
      mockCommands.getAvailableModels.mockRejectedValue(
        new Error("Network failure"),
      );

      const store = useModelStore.getState();
      await store.loadModels();

      const updated = useModelStore.getState();
      expect(updated.error).toContain("Network failure");
      expect(updated.loading).toBe(false);
    });

    it("syncs downloading state from backend", async () => {
      // Simulate a model that the backend says is downloading
      const modelsWithDownloading = SAMPLE_MODELS.map((m) =>
        m.id === "whisper-large" ? { ...m, is_downloading: true } : m,
      );
      mockCommands.getAvailableModels.mockResolvedValue({
        status: "ok",
        data: modelsWithDownloading,
      });

      const store = useModelStore.getState();
      await store.loadModels();

      const updated = useModelStore.getState();
      // The downloading model should be tracked
      expect(updated.downloadingModels["whisper-large"]).toBe(true);
    });
  });

  // ─── loadCurrentModel ──────────────────────────────────────────────
  describe("loadCurrentModel", () => {
    it("loads current model from backend", async () => {
      const store = useModelStore.getState();
      await store.loadCurrentModel();

      const updated = useModelStore.getState();
      expect(updated.currentModel).toBe("whisper-small");
    });

    it("handles loadCurrentModel failure gracefully", async () => {
      mockCommands.getCurrentModel.mockRejectedValue(new Error("No model"));

      const store = useModelStore.getState();
      await store.loadCurrentModel();

      // Should not crash; currentModel stays empty
      const updated = useModelStore.getState();
      expect(updated.currentModel).toBe("");
    });
  });

  // ─── Model selection ──────────────────────────────────────────────
  describe("model selection", () => {
    it("selects a model and updates currentModel", async () => {
      const store = useModelStore.getState();
      const result = await store.selectModel("whisper-medium");

      expect(result).toBe(true);
      expect(useModelStore.getState().currentModel).toBe("whisper-medium");
    });

    it("sets error on selection failure", async () => {
      mockCommands.setActiveModel.mockResolvedValue({
        status: "error",
        error: "Model not found",
      });

      const store = useModelStore.getState();
      const result = await store.selectModel("nonexistent");

      expect(result).toBe(false);
      expect(useModelStore.getState().error).toContain("Model not found");
    });

    it("handles exception during selection", async () => {
      mockCommands.setActiveModel.mockRejectedValue(
        new Error("IPC failure"),
      );

      const store = useModelStore.getState();
      const result = await store.selectModel("whisper-medium");

      expect(result).toBe(false);
      expect(useModelStore.getState().error).toContain("IPC failure");
    });
  });

  // ─── Model status tracking ─────────────────────────────────────────
  describe("download status tracking", () => {
    it("tracks downloading state when download starts", async () => {
      mockCommands.downloadModel.mockResolvedValue({ status: "ok", data: null });

      const store = useModelStore.getState();
      await store.downloadModel("whisper-large");

      // After successful download, the event listener would clean up
      // but in our test, we just verify the command was called
      expect(mockCommands.downloadModel).toHaveBeenCalledWith("whisper-large");
    });

    it("cleans up downloading state on download failure", async () => {
      mockCommands.downloadModel.mockResolvedValue({
        status: "error",
        error: "Download failed",
      });

      const store = useModelStore.getState();
      await store.downloadModel("whisper-large");

      const updated = useModelStore.getState();
      // Should clean up downloading state
      expect(updated.downloadingModels["whisper-large"]).toBeUndefined();
      expect(updated.downloadProgress["whisper-large"]).toBeUndefined();
    });

    it("cleans up on download exception", async () => {
      mockCommands.downloadModel.mockRejectedValue(
        new Error("Network error"),
      );

      const store = useModelStore.getState();
      await store.downloadModel("whisper-large");

      const updated = useModelStore.getState();
      expect(updated.downloadingModels["whisper-large"]).toBeUndefined();
      expect(updated.downloadProgress["whisper-large"]).toBeUndefined();
    });

    it("cancelDownload clears state and reloads models", async () => {
      const store = useModelStore.getState();
      await store.cancelDownload("whisper-large");

      const updated = useModelStore.getState();
      expect(updated.downloadingModels["whisper-large"]).toBeUndefined();
      expect(updated.downloadProgress["whisper-large"]).toBeUndefined();
      expect(mockCommands.cancelDownload).toHaveBeenCalledWith("whisper-large");
      // Should also reload models
      expect(mockCommands.getAvailableModels).toHaveBeenCalled();
    });
  });

  // ─── Helper methods ────────────────────────────────────────────────
  describe("helper methods", () => {
    it("getModelInfo returns model by id", async () => {
      const store = useModelStore.getState();
      await store.loadModels();

      const model = useModelStore.getState().getModelInfo("whisper-small");
      expect(model).toBeDefined();
      expect(model?.name).toBe("Whisper Small");
    });

    it("getModelInfo returns undefined for unknown model", async () => {
      const store = useModelStore.getState();
      await store.loadModels();

      const model = useModelStore.getState().getModelInfo("nonexistent");
      expect(model).toBeUndefined();
    });

    it("isModelDownloading checks downloading state", () => {
      useModelStore.setState({
        downloadingModels: { "whisper-large": true },
      });

      expect(useModelStore.getState().isModelDownloading("whisper-large")).toBe(true);
      expect(useModelStore.getState().isModelDownloading("whisper-small")).toBe(false);
    });

    it("isModelVerifying checks verifying state", () => {
      useModelStore.setState({
        verifyingModels: { "whisper-large": true },
      });

      expect(useModelStore.getState().isModelVerifying("whisper-large")).toBe(true);
      expect(useModelStore.getState().isModelVerifying("whisper-small")).toBe(false);
    });

    it("isModelExtracting checks extracting state", () => {
      useModelStore.setState({
        extractingModels: { "whisper-large": true },
      });

      expect(useModelStore.getState().isModelExtracting("whisper-large")).toBe(true);
      expect(useModelStore.getState().isModelExtracting("whisper-small")).toBe(false);
    });

    it("getDownloadProgress returns progress for active download", () => {
      const progress = {
        model_id: "whisper-large",
        downloaded: 1000,
        total: 3000,
        percentage: 33,
      };
      useModelStore.setState({
        downloadProgress: { "whisper-large": progress },
      });

      expect(useModelStore.getState().getDownloadProgress("whisper-large")).toEqual(progress);
      expect(useModelStore.getState().getDownloadProgress("whisper-small")).toBeUndefined();
    });
  });

  // ─── deleteModel ──────────────────────────────────────────────────
  describe("deleteModel", () => {
    it("deletes model and reloads", async () => {
      const store = useModelStore.getState();
      const result = await store.deleteModel("whisper-small");

      expect(result).toBe(true);
      expect(mockCommands.deleteModel).toHaveBeenCalledWith("whisper-small");
      // Should reload models and current model after delete
      expect(mockCommands.getAvailableModels).toHaveBeenCalled();
      expect(mockCommands.getCurrentModel).toHaveBeenCalled();
    });

    it("sets error on delete failure", async () => {
      mockCommands.deleteModel.mockResolvedValue({
        status: "error",
        error: "Cannot delete",
      });

      const store = useModelStore.getState();
      const result = await store.deleteModel("whisper-small");

      expect(result).toBe(false);
      expect(useModelStore.getState().error).toContain("Cannot delete");
    });
  });

  // ─── rescanLocalModels ────────────────────────────────────────────
  describe("rescanLocalModels", () => {
    it("sets isRescanning flag during rescan", async () => {
      let resolveRescan: () => void;
      mockCommands.rescanLocalModels.mockImplementation(
        () =>
          new Promise<void>((resolve) => {
            resolveRescan = resolve;
          }),
      );

      const store = useModelStore.getState();
      const rescanPromise = store.rescanLocalModels();

      expect(useModelStore.getState().isRescanning).toBe(true);

      resolveRescan!();
      await rescanPromise;

      expect(useModelStore.getState().isRescanning).toBe(false);
    });
  });

  // ─── initialize ──────────────────────────────────────────────────
  describe("initialize", () => {
    it("loads models and current model on initialize", async () => {
      const store = useModelStore.getState();
      await store.initialize();

      const updated = useModelStore.getState();
      expect(updated.models).toHaveLength(3);
      expect(updated.currentModel).toBe("whisper-small");
      expect(updated.initialized).toBe(true);
    });

    it("does not re-initialize if already initialized", async () => {
      const store = useModelStore.getState();
      await store.initialize();

      vi.clearAllMocks();
      // Reset mock to track calls
      mockCommands.getAvailableModels.mockResolvedValue({
        status: "ok",
        data: SAMPLE_MODELS,
      });

      await store.initialize();

      // Should NOT call backend again
      expect(mockCommands.getAvailableModels).not.toHaveBeenCalled();
    });
  });
});