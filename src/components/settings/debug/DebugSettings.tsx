import React from "react";
import { useTranslation } from "react-i18next";
import { WordCorrectionThreshold } from "./WordCorrectionThreshold";
import { LogLevelSelector } from "./LogLevelSelector";
import { LiveLogViewer } from "./LiveLogViewer";
import { PasteDelay } from "./PasteDelay";
import { PreRecordingBuffer } from "./PreRecordingBuffer";
import { RecordingBuffer } from "./RecordingBuffer";
import { LogDirectory } from "./LogDirectory";
import { SettingsGroup } from "../../ui/SettingsGroup";
import { AlwaysOnMicrophone } from "../AlwaysOnMicrophone";
import { SoundPicker } from "../SoundPicker";
import { ClamshellMicrophoneSelector } from "../ClamshellMicrophoneSelector";
import { UpdateChecksToggle } from "../UpdateChecksToggle";
import { UsbWatchdog } from "./UsbWatchdog";
import { RepetitionSuppressionSettings } from "./RepetitionSuppressionSettings";
import { WhatsNewPreview } from "./WhatsNewPreview";

export const DebugSettings: React.FC = () => {
  const { t } = useTranslation();

  return (
    <div className="max-w-3xl w-full mx-auto space-y-6">
      <SettingsGroup title={t("settings.debug.title")}>
        <LogLevelSelector grouped={true} />
        <LogDirectory descriptionMode="tooltip" grouped={true} />
        <WhatsNewPreview descriptionMode="tooltip" grouped={true} />
        <UpdateChecksToggle descriptionMode="tooltip" grouped={true} />
        <SoundPicker
          label={t("settings.debug.soundTheme.label")}
          description={t("settings.debug.soundTheme.description")}
        />
        <PasteDelay descriptionMode="tooltip" grouped={true} />
        <PreRecordingBuffer descriptionMode="tooltip" grouped={true} />
        <RecordingBuffer descriptionMode="tooltip" grouped={true} />
        <AlwaysOnMicrophone descriptionMode="tooltip" grouped={true} />
        <ClamshellMicrophoneSelector descriptionMode="tooltip" grouped={true} />
        <UsbWatchdog descriptionMode="tooltip" grouped={true} />
      </SettingsGroup>
      <RepetitionSuppressionSettings />
    </div>
  );
};