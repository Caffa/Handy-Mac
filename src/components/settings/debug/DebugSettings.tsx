import React from "react";
import { useTranslation } from "react-i18next";
import { WordCorrectionThreshold } from "./WordCorrectionThreshold";
import { AdvancedCustomWords } from "./AdvancedCustomWords";
import { LogLevelSelector } from "./LogLevelSelector";
import { PasteDelay } from "./PasteDelay";
import { RecordingBuffer } from "./RecordingBuffer";
import { SettingsGroup } from "../../ui/SettingsGroup";
import { AlwaysOnMicrophone } from "../AlwaysOnMicrophone";
import { SoundPicker } from "../SoundPicker";
import { ClamshellMicrophoneSelector } from "../ClamshellMicrophoneSelector";
import { UpdateChecksToggle } from "../UpdateChecksToggle";
import { UsbWatchdog } from "./UsbWatchdog";
import { ToggleSwitch } from "../../ui/ToggleSwitch";
import { useSettings } from "../../../hooks/useSettings";

export const DebugSettings: React.FC = () => {
  const { t } = useTranslation();
  const { getSetting, updateSetting, isUpdating } = useSettings();
  const useAdvancedCustomWords = getSetting("use_advanced_custom_words") || false;

  return (
    <div className="max-w-3xl w-full mx-auto space-y-6">
      <SettingsGroup title={t("settings.debug.title")}>
        <LogLevelSelector grouped={true} />
        <UpdateChecksToggle descriptionMode="tooltip" grouped={true} />
        <SoundPicker
          label={t("settings.debug.soundTheme.label")}
          description={t("settings.debug.soundTheme.description")}
        />
        <ToggleSwitch
          checked={useAdvancedCustomWords}
          onChange={(checked) => updateSetting("use_advanced_custom_words", checked)}
          disabled={isUpdating("use_advanced_custom_words")}
          isUpdating={isUpdating("use_advanced_custom_words")}
          label={t("settings.debug.advancedCustomWords.toggleLabel")}
          description={t("settings.debug.advancedCustomWords.toggleDescription")}
          descriptionMode="tooltip"
          grouped={true}
        />
        <WordCorrectionThreshold descriptionMode="tooltip" grouped={true} />
        <PasteDelay descriptionMode="tooltip" grouped={true} />
        <RecordingBuffer descriptionMode="tooltip" grouped={true} />
        <AlwaysOnMicrophone descriptionMode="tooltip" grouped={true} />
        <ClamshellMicrophoneSelector descriptionMode="tooltip" grouped={true} />
        <UsbWatchdog descriptionMode="tooltip" grouped={true} />
      </SettingsGroup>

      {useAdvancedCustomWords && (
        <SettingsGroup title={t("settings.debug.advancedCustomWords.title")}>
          <AdvancedCustomWords descriptionMode="tooltip" grouped={true} />
        </SettingsGroup>
      )}
    </div>
  );
};
