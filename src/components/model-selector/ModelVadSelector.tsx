import React, { useMemo } from "react";
import { useTranslation } from "react-i18next";
import type { ModelInfo, VadSensitivity } from "@/bindings";
import { useSettings } from "@/hooks/useSettings";
import { Dropdown } from "@/components/ui/Dropdown";
import type { DropdownOption } from "@/components/ui/Dropdown";

interface ModelVadSelectorProps {
  model: ModelInfo;
  compact?: boolean;
}

const VALID_SENSITIVITIES: VadSensitivity[] = [
  "very_relaxed",
  "relaxed",
  "balanced",
  "quick",
  "very_quick",
];

// The engine type is gone from ModelInfo (every model loads through
// transcribe-cpp now); the Parakeet architecture is recognized by name, in
// step with the backend's `VadSensitivity::default_for_model`. Both the
// retired ONNX ids and the catalog GGUF ids carry it.
const isParakeetModel = (model: ModelInfo): boolean =>
  model.id.toLowerCase().includes("parakeet");

const getEngineDefault = (model: ModelInfo): VadSensitivity =>
  isParakeetModel(model) ? "very_relaxed" : "balanced";

const getEngineLabelKey = (model: ModelInfo): string =>
  isParakeetModel(model) ? "Parakeet" : "Whisper";

const getSensitivityLabelKey = (value: VadSensitivity): string => {
  switch (value) {
    case "very_relaxed":
      return "settings.advanced.vadSensitivity.levels.none";
    case "relaxed":
      return "settings.advanced.vadSensitivity.levels.light";
    case "balanced":
      return "settings.advanced.vadSensitivity.levels.balanced";
    case "quick":
      return "settings.advanced.vadSensitivity.levels.aggressive";
    case "very_quick":
      return "settings.advanced.vadSensitivity.levels.strongAggressive";
    default:
      return "settings.advanced.vadSensitivity.levels.balanced";
  }
};

export const ModelVadSelector: React.FC<ModelVadSelectorProps> = ({
  model,
  compact = false,
}) => {
  const { t } = useTranslation();
  const { settings, setModelVadSensitivity, isUpdating } = useSettings();

  const overrides = settings?.per_model_vad_sensitivity ?? {};
  const savedValue = overrides[model.id];
  const effectiveValue = savedValue ?? getEngineDefault(model);
  const isOverridden = savedValue !== undefined;

  const options: DropdownOption[] = useMemo(
    () =>
      VALID_SENSITIVITIES.map((value) => ({
        value,
        label: t(getSensitivityLabelKey(value)),
      })),
    [t],
  );

  const handleSelect = async (value: string) => {
    if (value === effectiveValue && isOverridden) return;
    await setModelVadSensitivity(model.id, value as VadSensitivity);
  };

  const handleReset = async () => {
    await setModelVadSensitivity(model.id, null);
  };

  const isBusy = isUpdating(`model_vad_sensitivity:${model.id}`);

  return (
    <div
      className={`flex items-center gap-2 ${compact ? "text-xs" : "text-sm"}`}
      title={
        isOverridden
          ? t("modelSelector.vadSensitivity.default", {
              engine: getEngineLabelKey(model),
            })
          : undefined
      }
    >
      <span className="text-text/60 whitespace-nowrap">
        {t("modelSelector.vadSensitivity.label")}:
      </span>
      <Dropdown
        options={options}
        selectedValue={effectiveValue}
        onSelect={handleSelect}
        disabled={isBusy}
        className={compact ? "w-40" : "w-48"}
      />
      {isOverridden && (
        <button
          type="button"
          onClick={handleReset}
          disabled={isBusy}
          className="text-text/50 hover:text-logo-primary disabled:opacity-50 transition-colors"
          title={t("modelSelector.vadSensitivity.resetToDefault")}
        >
          <svg
            className="w-3.5 h-3.5"
            fill="none"
            stroke="currentColor"
            viewBox="0 0 24 24"
          >
            <path
              strokeLinecap="round"
              strokeLinejoin="round"
              strokeWidth={2}
              d="M4 4v5h.582m15.356 2A8.001 8.001 0 004.582 9m0 0H9m11 11v-5h-.581m0 0a8.003 8.003 0 01-15.357-2m15.357 2H15"
            />
          </svg>
        </button>
      )}
    </div>
  );
};
