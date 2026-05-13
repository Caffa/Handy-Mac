import React, { useEffect, useState } from "react";
import { useTranslation } from "react-i18next";
import { toast } from "sonner";
import { listen } from "@tauri-apps/api/event";
import type { ModelInfo } from "@/bindings";
import type { ModelCardStatus } from "./ModelCard";
import ModelCard from "./ModelCard";
import HandyTextLogo from "../icons/HandyTextLogo";
import { useModelStore } from "../../stores/modelStore";

interface OnboardingProps {
  onModelSelected: () => void;
}

const Onboarding: React.FC<OnboardingProps> = ({ onModelSelected }) => {
  const { t } = useTranslation();
  const {
    models,
    downloadModel,
    cancelDownload,
    selectModel,
    downloadingModels,
    verifyingModels,
    extractingModels,
    downloadProgress,
    downloadStats,
  } = useModelStore();
  const [selectedModelId, setSelectedModelId] = useState<string | null>(null);

  const isDownloading = selectedModelId !== null;

  const downloadableModels = models.filter((m: ModelInfo) => !m.is_downloaded);
  const hasNoModelsToDownload = downloadableModels.length === 0 && !isDownloading;

  // If all models are already downloaded, skip onboarding immediately
  useEffect(() => {
    if (hasNoModelsToDownload) {
      onModelSelected();
    }
  }, [hasNoModelsToDownload, onModelSelected]);

  // Listen for download failures to clear the stuck disabled state.
  // When a download starts, `handleDownloadModel` sets `selectedModelId`.
  // If the download request succeeds (returns true) but the download later
  // fails asynchronously (network error, checksum mismatch, disk full),
  // the `model-download-failed` event fires but `selectedModelId` was never
  // cleared, leaving all model cards permanently disabled.
  useEffect(() => {
    const unlistenFailed = listen<{ model_id: string; error: string }>(
      "model-download-failed",
      (event) => {
        if (event.payload.model_id === selectedModelId) {
          setSelectedModelId(null);
        }
      },
    );
    const unlistenCancelled = listen<string>(
      "model-download-cancelled",
      (modelId) => {
        if (modelId.payload === selectedModelId) {
          setSelectedModelId(null);
        }
      },
    );

    return () => {
      unlistenFailed.then((fn) => fn());
      unlistenCancelled.then((fn) => fn());
    };
  }, [selectedModelId]);

  // Watch for the selected model to finish downloading + verifying + extracting
  useEffect(() => {
    if (!selectedModelId) return;

    const model = models.find((m) => m.id === selectedModelId);
    const stillDownloading = selectedModelId in downloadingModels;
    const stillVerifying = selectedModelId in verifyingModels;
    const stillExtracting = selectedModelId in extractingModels;

    if (
      model?.is_downloaded &&
      !stillDownloading &&
      !stillVerifying &&
      !stillExtracting
    ) {
      // Model is ready — select it and transition
      selectModel(selectedModelId).then((success) => {
        if (success) {
          onModelSelected();
        } else {
          toast.error(t("onboarding.errors.selectModel"));
          setSelectedModelId(null);
        }
      });
    }
  }, [
    selectedModelId,
    models,
    downloadingModels,
    verifyingModels,
    extractingModels,
    selectModel,
    onModelSelected,
    t,
  ]);

  const handleDownloadModel = async (modelId: string) => {
    setSelectedModelId(modelId);

    // Error toast is handled centrally by the model-download-failed event listener
    // in modelStore — no toast here to avoid duplicates.
    const success = await downloadModel(modelId);
    if (!success) {
      // Download request itself failed (IPC error), clear selected state.
      // For async download failures, the model-download-failed listener above
      // handles clearing selectedModelId.
      setSelectedModelId(null);
    }
  };

  const handleCancelDownload = async () => {
    if (!selectedModelId) return;
    const modelId = selectedModelId;
    setSelectedModelId(null);
    await cancelDownload(modelId);
  };

  const getModelStatus = (modelId: string): ModelCardStatus => {
    if (modelId in extractingModels) return "extracting";
    if (modelId in verifyingModels) return "verifying";
    if (modelId in downloadingModels) return "downloading";
    return "downloadable";
  };

  const getModelDownloadProgress = (modelId: string): number | undefined => {
    return downloadProgress[modelId]?.percentage;
  };

  const getModelDownloadSpeed = (modelId: string): number | undefined => {
    return downloadStats[modelId]?.speed;
  };

  return (
    <div className="h-screen w-screen flex flex-col p-6 gap-4 inset-0">
      <div className="flex flex-col items-center gap-2 shrink-0">
        <HandyTextLogo width={200} />
        <p className="text-text/70 max-w-md font-medium mx-auto">
          {t("onboarding.subtitle")}
        </p>
      </div>

      <div className="max-w-[600px] w-full mx-auto text-center flex-1 flex flex-col min-h-0">
        <div className="flex flex-col gap-4 pb-6">
          {models
            .filter((m: ModelInfo) => !m.is_downloaded)
            .filter((model: ModelInfo) => model.is_recommended)
            .map((model: ModelInfo) => (
              <ModelCard
                key={model.id}
                model={model}
                variant="featured"
                status={getModelStatus(model.id)}
                disabled={isDownloading && model.id !== selectedModelId}
                onSelect={handleDownloadModel}
                onDownload={handleDownloadModel}
                onCancel={handleCancelDownload}
                downloadProgress={getModelDownloadProgress(model.id)}
                downloadSpeed={getModelDownloadSpeed(model.id)}
              />
            ))}

          {models
            .filter((m: ModelInfo) => !m.is_downloaded)
            .filter((model: ModelInfo) => !model.is_recommended)
            .sort(
              (a: ModelInfo, b: ModelInfo) =>
                Number(a.size_mb) - Number(b.size_mb),
            )
            .map((model: ModelInfo) => (
              <ModelCard
                key={model.id}
                model={model}
                status={getModelStatus(model.id)}
                disabled={isDownloading && model.id !== selectedModelId}
                onSelect={handleDownloadModel}
                onDownload={handleDownloadModel}
                onCancel={handleCancelDownload}
                downloadProgress={getModelDownloadProgress(model.id)}
                downloadSpeed={getModelDownloadSpeed(model.id)}
              />
            ))}
        </div>
      </div>
    </div>
  );
};

export default Onboarding;