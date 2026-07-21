import React from "react";
import { useTranslation } from "react-i18next";
import { SettingsGroup } from "../../ui/SettingsGroup";
import { useSettings } from "../../../hooks/useSettings";
import { WordCorrectionModeSelector } from "../WordCorrectionModeSelector";
import { CustomWords } from "../CustomWords";
import { CustomFillerWords } from "../CustomFillerWords";
import { WordCorrectionThreshold } from "../WordCorrectionThreshold";
import { AdvancedCustomWords } from "../AdvancedCustomWords";
import { WordReplacements } from "../WordReplacements";
import { VadSensitivity } from "../VadSensitivity";
import { LiveCaptions } from "../LiveCaptions";
import { ConvertUsToBritish } from "../ConvertUsToBritish";
import { TranslateToEnglish } from "../TranslateToEnglish";
import { NoiseSuppression } from "../NoiseSuppression";

export const AccuracySettings: React.FC = () => {
  const { t } = useTranslation();
  const { getSetting } = useSettings();
  const wordCorrectionMode = getSetting("word_correction_mode") || "word_bias";

  return (
    <div className="max-w-3xl w-full mx-auto space-y-6">
      <SettingsGroup title={t("settings.accuracy.groups.wordCorrection")}>
        <WordCorrectionModeSelector descriptionMode="tooltip" grouped />
        {wordCorrectionMode === "word_bias" ? (
          <>
            <CustomWords descriptionMode="tooltip" grouped />
            <CustomFillerWords descriptionMode="tooltip" grouped />
            <WordCorrectionThreshold descriptionMode="tooltip" grouped />
          </>
        ) : wordCorrectionMode === "pronunciation" ? (
          <AdvancedCustomWords descriptionMode="tooltip" grouped />
        ) : (
          <WordReplacements descriptionMode="tooltip" grouped />
        )}
      </SettingsGroup>

      <SettingsGroup title={t("settings.accuracy.groups.voiceDetection")}>
        <VadSensitivity descriptionMode="tooltip" grouped />
        <NoiseSuppression />
        <LiveCaptions descriptionMode="tooltip" grouped />
      </SettingsGroup>

      <SettingsGroup title={t("settings.accuracy.groups.language")}>
        <TranslateToEnglish descriptionMode="tooltip" grouped />
        <ConvertUsToBritish descriptionMode="tooltip" grouped />
      </SettingsGroup>
    </div>
  );
};
