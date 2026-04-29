import React, { useEffect, useMemo, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { ask } from "@tauri-apps/plugin-dialog";
import { AlertTriangle, ChevronDown, Globe, Gauge, Loader2, Trash2 } from "lucide-react";
import type { ModelCardStatus } from "@/components/onboarding";
import { ModelCard } from "@/components/onboarding";
import { useModelStore } from "@/stores/modelStore";
import { useSettings } from "../../../hooks/useSettings";
import { LANGUAGES } from "@/lib/constants/languages.ts";
import { commands } from "@/bindings";
import type { BenchmarkModelFailure, ModelInfo } from "@/bindings";

// check if model supports a language based on its supported_languages list
const modelSupportsLanguage = (model: ModelInfo, langCode: string): boolean => {
  return model.supported_languages.includes(langCode);
};

export const ModelsSettings: React.FC = () => {
  const { t } = useTranslation();
  const [switchingModelId, setSwitchingModelId] = useState<string | null>(null);
  const [languageFilter, setLanguageFilter] = useState("all");
  const [languageDropdownOpen, setLanguageDropdownOpen] = useState(false);
  const [languageSearch, setLanguageSearch] = useState("");
  const [benchmarkRunning, setBenchmarkRunning] = useState(false);
  const [benchmarkProgress, setBenchmarkProgress] = useState<{
    stage: string;
    model_name?: string;
    progress?: number;
  } | null>(null);
  const [clipCount, setClipCount] = useState<number>(0);
  const [benchmarkFailures, setBenchmarkFailures] = useState<
    BenchmarkModelFailure[]
  >([]);
  const [benchmarkSkipped, setBenchmarkSkipped] = useState<number>(0);
  const languageDropdownRef = useRef<HTMLDivElement>(null);
  const languageSearchInputRef = useRef<HTMLInputElement>(null);
  const {
    models,
    currentModel,
    downloadingModels,
    downloadProgress,
    downloadStats,
    verifyingModels,
    extractingModels,
    loading,
    downloadModel,
    cancelDownload,
    selectModel,
    deleteModel,
    loadModels,
  } = useModelStore();
  const { getSetting } = useSettings();

  const hybridModeEnabled = getSetting("hybrid_mode_enabled") ?? false;
  const hybridShortModel = getSetting("hybrid_short_audio_model") ?? null;
  const hybridLongModel = getSetting("hybrid_long_audio_model") ?? null;

  // Build hybrid roles map: model_id -> "short" | "long"
  const hybridRoles = useMemo<Record<string, "short" | "long">>(() => {
    if (!hybridModeEnabled) return {};
    const roles: Record<string, "short" | "long"> = {};
    if (hybridShortModel) roles[hybridShortModel] = "short";
    if (hybridLongModel) roles[hybridLongModel] = "long";
    return roles;
  }, [hybridModeEnabled, hybridShortModel, hybridLongModel]);

  // click outside handler for language dropdown
  useEffect(() => {
    const handleClickOutside = (event: MouseEvent) => {
      if (
        languageDropdownRef.current &&
        !languageDropdownRef.current.contains(event.target as Node)
      ) {
        setLanguageDropdownOpen(false);
        setLanguageSearch("");
      }
    };
    document.addEventListener("mousedown", handleClickOutside);
    return () => document.removeEventListener("mousedown", handleClickOutside);
  }, []);

  // focus search input when dropdown opens
  useEffect(() => {
    if (languageDropdownOpen && languageSearchInputRef.current) {
      languageSearchInputRef.current.focus();
    }
  }, [languageDropdownOpen]);

  // Check if benchmarking is available
  useEffect(() => {
    commands.getBenchmarkClipCount().then((result) => {
      if (result.status === "ok") {
        setClipCount(result.data);
      }
    });
  }, [models]);

  // Listen for benchmark progress events
  useEffect(() => {
    if (!benchmarkRunning) return;
    const unlisten = import("@tauri-apps/api/event").then(({ listen }) =>
      listen<{ stage: string; model_name?: string; progress?: number }>(
        "benchmark-progress",
        (event) => {
          setBenchmarkProgress(event.payload);
        },
      ),
    );
    return () => {
      unlisten.then((unlistenFn) => unlistenFn());
    };
  }, [benchmarkRunning]);

  const handleRunBenchmark = async () => {
    setBenchmarkRunning(true);
    setBenchmarkProgress({ stage: "started" });
    setBenchmarkFailures([]);
    setBenchmarkSkipped(0);
    try {
      const result = await commands.benchmarkModels();
      if (result.status === "ok") {
        setBenchmarkProgress({ stage: "completed" });
        setBenchmarkFailures(result.data.failed_models);
        setBenchmarkSkipped(result.data.skipped_model_ids.length);
        // Reload models to get updated dynamic_score
        await loadModels();
      } else {
        setBenchmarkProgress(null);
      }
    } catch {
      setBenchmarkProgress(null);
    } finally {
      setBenchmarkRunning(false);
    }
  };

  // filtered languages for dropdown (exclude "auto")
  const filteredLanguages = useMemo(() => {
    return LANGUAGES.filter(
      (lang) =>
        lang.value !== "auto" &&
        lang.label.toLowerCase().includes(languageSearch.toLowerCase()),
    );
  }, [languageSearch]);

  // Get selected language label
  const selectedLanguageLabel = useMemo(() => {
    if (languageFilter === "all") {
      return t("settings.models.filters.allLanguages");
    }
    return LANGUAGES.find((lang) => lang.value === languageFilter)?.label || "";
  }, [languageFilter, t]);

  const getModelStatus = (modelId: string): ModelCardStatus => {
    if (modelId in extractingModels) {
      return "extracting";
    }
    if (modelId in verifyingModels) {
      return "verifying";
    }
    if (modelId in downloadingModels) {
      return "downloading";
    }
    if (switchingModelId === modelId) {
      return "switching";
    }
    if (modelId === currentModel) {
      return "active";
    }
    const model = models.find((m: ModelInfo) => m.id === modelId);
    if (model?.is_downloaded) {
      return "available";
    }
    return "downloadable";
  };

  const getDownloadProgress = (modelId: string): number | undefined => {
    const progress = downloadProgress[modelId];
    return progress?.percentage;
  };

  const getDownloadSpeed = (modelId: string): number | undefined => {
    const stats = downloadStats[modelId];
    return stats?.speed;
  };

  const handleModelSelect = async (modelId: string) => {
    setSwitchingModelId(modelId);
    try {
      await selectModel(modelId);
    } finally {
      setSwitchingModelId(null);
    }
  };

  const handleModelDownload = async (modelId: string) => {
    await downloadModel(modelId);
  };

  const handleModelDelete = async (modelId: string) => {
    const model = models.find((m: ModelInfo) => m.id === modelId);
    const modelName = model?.name || modelId;
    const isActive = modelId === currentModel;

    const confirmed = await ask(
      isActive
        ? t("settings.models.deleteActiveConfirm", { modelName })
        : t("settings.models.deleteConfirm", { modelName }),
      {
        title: t("settings.models.deleteTitle"),
        kind: "warning",
      },
    );

    if (confirmed) {
      try {
        await deleteModel(modelId);
      } catch (err) {
        console.error(`Failed to delete model ${modelId}:`, err);
      }
    }
  };

  const handleModelCancel = async (modelId: string) => {
    try {
      await cancelDownload(modelId);
    } catch (err) {
      console.error(`Failed to cancel download for ${modelId}:`, err);
    }
  };

  // Filter models based on language filter
  const filteredModels = useMemo(() => {
    return models.filter((model: ModelInfo) => {
      if (languageFilter !== "all") {
        if (!modelSupportsLanguage(model, languageFilter)) return false;
      }
      return true;
    });
  }, [models, languageFilter]);

  // Split filtered models into downloaded (including custom) and available sections
  const { downloadedModels, availableModels } = useMemo(() => {
    const downloaded: ModelInfo[] = [];
    const available: ModelInfo[] = [];

    for (const model of filteredModels) {
      if (
        model.is_custom ||
        model.is_downloaded ||
        model.id in downloadingModels ||
        model.id in extractingModels
      ) {
        downloaded.push(model);
      } else {
        available.push(model);
      }
    }

    // Sort: active model first, then non-custom, then custom at the bottom
    downloaded.sort((a, b) => {
      if (a.id === currentModel) return -1;
      if (b.id === currentModel) return 1;
      if (a.is_custom !== b.is_custom) return a.is_custom ? 1 : -1;
      return 0;
    });

    return {
      downloadedModels: downloaded,
      availableModels: available,
    };
  }, [filteredModels, downloadingModels, extractingModels, currentModel]);

  if (loading) {
    return (
      <div className="max-w-3xl w-full mx-auto">
        <div className="flex items-center justify-center py-16">
          <div className="w-8 h-8 border-2 border-logo-primary border-t-transparent rounded-full animate-spin" />
        </div>
      </div>
    );
  }

  return (
    <div className="max-w-3xl w-full mx-auto space-y-4">
      <div className="mb-4">
        <h1 className="text-xl font-semibold mb-2">
          {t("settings.models.title")}
        </h1>
        <p className="text-sm text-text/60">
          {t("settings.models.description")}
        </p>
      </div>
      {/* Benchmark section */}
      <div className="border border-mid-gray/20 rounded-xl p-4 space-y-3">
        <div className="flex items-center justify-between">
          <div className="flex items-center gap-2">
            <Gauge className="w-4 h-4 text-logo-primary" />
            <h3 className="text-sm font-medium">
              {t("settings.models.benchmark.title")}
            </h3>
          </div>
          <button
            type="button"
            onClick={handleRunBenchmark}
            disabled={benchmarkRunning || clipCount < 20}
            className={`flex items-center gap-1.5 px-3 py-1.5 text-sm font-medium rounded-lg transition-colors ${
              benchmarkRunning || clipCount < 20
                ? "bg-mid-gray/10 text-text/30 cursor-not-allowed"
                : "bg-logo-primary/20 text-logo-primary hover:bg-logo-primary/30"
            }`}
          >
            {benchmarkRunning ? (
              <Loader2 className="w-3.5 h-3.5 animate-spin" />
            ) : (
              <Gauge className="w-3.5 h-3.5" />
            )}
            {benchmarkRunning
              ? t("settings.models.benchmark.running")
              : t("settings.models.benchmark.runBenchmark")}
          </button>
        </div>
        <p className="text-xs text-text/50">
          {clipCount < 20
            ? t("settings.models.benchmark.needMoreClips", {
                current: clipCount,
                needed: 20,
              })
            : t("settings.models.benchmark.canBenchmark", {
                count: clipCount,
              })}
        </p>
        {benchmarkProgress && benchmarkProgress.stage !== "completed" && (
          <div className="space-y-1">
            <div className="w-full h-1.5 bg-mid-gray/20 rounded-full overflow-hidden">
              <div
                className="h-full bg-logo-primary rounded-full transition-all duration-300 animate-pulse"
                style={{
                  width: `${Math.max(benchmarkProgress.progress ?? 5, 5)}%`,
                }}
              />
            </div>
            {benchmarkProgress.model_name && (
              <p className="text-xs text-text/50">
                {benchmarkProgress.stage === "loading"
                  ? `Loading ${benchmarkProgress.model_name}...`
                  : `Testing ${benchmarkProgress.model_name}...`}
              </p>
            )}
          </div>
        )}
        {benchmarkProgress?.stage === "completed" && (
          <div className="space-y-1.5">
            <p className="text-xs text-green-500 font-medium">
              {t("settings.models.benchmark.completedWithCheck")}{" "}
              {t("settings.models.benchmark.tooltip")}
            </p>
            {benchmarkSkipped > 0 && (
              <p className="text-xs text-text/50">
                {t("settings.models.benchmark.skipped", {
                  count: benchmarkSkipped,
                })}
              </p>
            )}
            {benchmarkFailures.length > 0 && (
              <div className="space-y-1">
                {benchmarkFailures.map((f) => (
                  <p
                    key={f.model_id}
                    className="text-xs text-red-400 flex items-center gap-1"
                  >
                    <AlertTriangle className="w-3 h-3 shrink-0" />
                    <span>
                      {f.model_name}:{" "}
                      {f.reason === "load"
                        ? t("settings.models.benchmark.failedLoad")
                        : t("settings.models.benchmark.failedTranscribe")}
                    </span>
                  </p>
                ))}
              </div>
            )}
          </div>
        )}
        {/* Show measured score indicator if any model has been benchmarked */}
        {models.some((m: ModelInfo) => m.dynamic_score && !m.dynamic_score.failed) && (
          <p className="text-xs text-text/40 italic">
            {t("settings.models.benchmark.tooltip")}
          </p>
        )}
      </div>
      {filteredModels.length > 0 ? (
        <div className="space-y-6">
          {/* Downloaded Models Section — header always visible so filter stays accessible */}
          <div className="space-y-3">
            <div className="flex items-center justify-between">
              <h2 className="text-sm font-medium text-text/60">
                {t("settings.models.yourModels")}
              </h2>
              {/* Language filter dropdown */}
              <div className="relative" ref={languageDropdownRef}>
                <button
                  type="button"
                  onClick={() => setLanguageDropdownOpen(!languageDropdownOpen)}
                  className={`flex items-center gap-1.5 px-3 py-1.5 text-sm font-medium rounded-lg transition-colors ${
                    languageFilter !== "all"
                      ? "bg-logo-primary/20 text-logo-primary"
                      : "bg-mid-gray/10 text-text/60 hover:bg-mid-gray/20"
                  }`}
                >
                  <Globe className="w-3.5 h-3.5" />
                  <span className="max-w-[120px] truncate">
                    {selectedLanguageLabel}
                  </span>
                  <ChevronDown
                    className={`w-3.5 h-3.5 transition-transform ${
                      languageDropdownOpen ? "rotate-180" : ""
                    }`}
                  />
                </button>

                {languageDropdownOpen && (
                  <div className="absolute top-full right-0 mt-1 w-56 bg-background border border-mid-gray/80 rounded-lg shadow-lg z-50 overflow-hidden">
                    <div className="p-2 border-b border-mid-gray/40">
                      <input
                        ref={languageSearchInputRef}
                        type="text"
                        value={languageSearch}
                        onChange={(e) => setLanguageSearch(e.target.value)}
                        onKeyDown={(e) => {
                          if (
                            e.key === "Enter" &&
                            filteredLanguages.length > 0
                          ) {
                            setLanguageFilter(filteredLanguages[0].value);
                            setLanguageDropdownOpen(false);
                            setLanguageSearch("");
                          } else if (e.key === "Escape") {
                            setLanguageDropdownOpen(false);
                            setLanguageSearch("");
                          }
                        }}
                        placeholder={t(
                          "settings.general.language.searchPlaceholder",
                        )}
                        className="w-full px-2 py-1 text-sm bg-mid-gray/10 border border-mid-gray/40 rounded-md focus:outline-none focus:ring-1 focus:ring-logo-primary"
                      />
                    </div>
                    <div className="max-h-48 overflow-y-auto">
                      <button
                        type="button"
                        onClick={() => {
                          setLanguageFilter("all");
                          setLanguageDropdownOpen(false);
                          setLanguageSearch("");
                        }}
                        className={`w-full px-3 py-1.5 text-sm text-left transition-colors ${
                          languageFilter === "all"
                            ? "bg-logo-primary/20 text-logo-primary font-semibold"
                            : "hover:bg-mid-gray/10"
                        }`}
                      >
                        {t("settings.models.filters.allLanguages")}
                      </button>
                      {filteredLanguages.map((lang) => (
                        <button
                          key={lang.value}
                          type="button"
                          onClick={() => {
                            setLanguageFilter(lang.value);
                            setLanguageDropdownOpen(false);
                            setLanguageSearch("");
                          }}
                          className={`w-full px-3 py-1.5 text-sm text-left transition-colors ${
                            languageFilter === lang.value
                              ? "bg-logo-primary/20 text-logo-primary font-semibold"
                              : "hover:bg-mid-gray/10"
                          }`}
                        >
                          {lang.label}
                        </button>
                      ))}
                      {filteredLanguages.length === 0 && (
                        <div className="px-3 py-2 text-sm text-text/50 text-center">
                          {t("settings.general.language.noResults")}
                        </div>
                      )}
                    </div>
                  </div>
                )}
              </div>
            </div>
            {downloadedModels.map((model: ModelInfo) => (
              <ModelCard
                key={model.id}
                model={model}
                status={getModelStatus(model.id)}
                onSelect={handleModelSelect}
                onDownload={handleModelDownload}
                onDelete={handleModelDelete}
                onCancel={handleModelCancel}
                downloadProgress={getDownloadProgress(model.id)}
                downloadSpeed={getDownloadSpeed(model.id)}
                showRecommended={false}
                hybridRole={hybridRoles[model.id] ?? null}
              />
            ))}
          </div>

          {/* Available Models Section */}
          {availableModels.length > 0 && (
            <div className="space-y-3">
              <h2 className="text-sm font-medium text-text/60">
                {t("settings.models.availableModels")}
              </h2>
              {availableModels.map((model: ModelInfo) => (
                <ModelCard
                  key={model.id}
                  model={model}
                  status={getModelStatus(model.id)}
                  onSelect={handleModelSelect}
                  onDownload={handleModelDownload}
                  onDelete={handleModelDelete}
                  onCancel={handleModelCancel}
                  downloadProgress={getDownloadProgress(model.id)}
                  downloadSpeed={getDownloadSpeed(model.id)}
                  showRecommended={false}
                  hybridRole={hybridRoles[model.id] ?? null}
                />
              ))}
            </div>
          )}
        </div>
      ) : (
        <div className="text-center py-8 text-text/50">
          {t("settings.models.noModelsMatch")}
        </div>
      )}
    </div>
  );
};
