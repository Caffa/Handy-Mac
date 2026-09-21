import React from "react";
import { useTranslation } from "react-i18next";
import { ToggleSwitch } from "../ui/ToggleSwitch";
import { useSettings } from "../../hooks/useSettings";

interface ConvertNumberWordsToDigitsProps {
  descriptionMode?: "inline" | "tooltip";
  grouped?: boolean;
}

export const ConvertNumberWordsToDigits: React.FC<ConvertNumberWordsToDigitsProps> =
  React.memo(({ descriptionMode = "tooltip", grouped = false }) => {
    const { t } = useTranslation();
    const { getSetting, updateSetting, isUpdating } = useSettings();

    const convertNumberWordsToDigits =
      getSetting("convert_number_words_to_digits") || false;

    return (
      <ToggleSwitch
        checked={convertNumberWordsToDigits}
        onChange={(enabled) =>
          updateSetting("convert_number_words_to_digits", enabled)
        }
        isUpdating={isUpdating("convert_number_words_to_digits")}
        label={t("settings.advanced.convertNumberWordsToDigits.label")}
        description={t(
          "settings.advanced.convertNumberWordsToDigits.description",
        )}
        descriptionMode={descriptionMode}
        grouped={grouped}
      />
    );
  });
