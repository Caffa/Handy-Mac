import React from "react";
import { useTranslation } from "react-i18next";
import { type } from "@tauri-apps/plugin-os";
import { MicrophoneSelector } from "../MicrophoneSelector";
import { ChannelSelector } from "../ChannelSelector";
import { ShortcutInput } from "../ShortcutInput";
import { SettingsGroup } from "../../ui/SettingsGroup";
import { OutputDeviceSelector } from "../OutputDeviceSelector";
import { PushToTalk } from "../PushToTalk";
import { AudioFeedback } from "../AudioFeedback";
import { useSettings } from "../../../hooks/useSettings";
import { VolumeSlider } from "../VolumeSlider";
import { MuteWhileRecording } from "../MuteWhileRecording";
import { LanguageSelector } from "../LanguageSelector";
import { AppLanguageSelector } from "../AppLanguageSelector";
import { PreRecordingBuffer } from "../debug/PreRecordingBuffer";
import { RecordingBuffer } from "../debug/RecordingBuffer";
import { useModelStore } from "../../../stores/modelStore";
import { MicReadinessSelector } from "./MicReadinessSelector";
import type { ModelInfo } from "@/bindings";

export const GeneralSettings: React.FC = () => {
  const { t } = useTranslation();
  const { audioFeedbackEnabled, getSetting } = useSettings();
  const pushToTalk = getSetting("push_to_talk");
  const isLinux = type() === "linux";

  const { currentModel, models } = useModelStore();
  const currentModelInfo = models.find((m: ModelInfo) => m.id === currentModel);
  const supportsLanguageSelection =
    currentModelInfo?.supports_language_selection ?? false;

  return (
    <div className="max-w-3xl w-full mx-auto space-y-6">
      <SettingsGroup title={t("settings.general.language.title")}>
        {supportsLanguageSelection && currentModelInfo && (
          <LanguageSelector
            descriptionMode="tooltip"
            grouped={true}
            supportedLanguages={currentModelInfo.supported_languages}
          />
        )}
        <AppLanguageSelector descriptionMode="tooltip" grouped={true} />
      </SettingsGroup>

      <SettingsGroup title={t("settings.general.shortcut.title")}>
        <ShortcutInput shortcutId="transcribe" grouped={true} />
        <ShortcutInput shortcutId="transcribe_with_router" grouped={true} />
        <PushToTalk descriptionMode="tooltip" grouped={true} />
        {/* Cancel shortcut is hidden with push-to-talk (release key cancels) and on Linux (dynamic shortcut instability) */}
        {!isLinux && !pushToTalk && (
          <ShortcutInput shortcutId="cancel" grouped={true} />
        )}
      </SettingsGroup>

      <SettingsGroup title={t("settings.general.micReadiness.title")}>
        <MicReadinessSelector descriptionMode="tooltip" grouped={true} />
      </SettingsGroup>

      <SettingsGroup title={t("settings.general.capturePadding.title")}>
        <PreRecordingBuffer
          descriptionMode="tooltip"
          grouped={true}
          disabledHint={t(
            "settings.general.capturePadding.preRecordingDisabledHint",
          )}
        />
        <RecordingBuffer descriptionMode="tooltip" grouped={true} />
      </SettingsGroup>

      <SettingsGroup title={t("settings.sound.title")}>
        <MicrophoneSelector descriptionMode="tooltip" grouped={true} />
        <ChannelSelector descriptionMode="tooltip" grouped={true} />
        <MuteWhileRecording descriptionMode="tooltip" grouped={true} />
        <AudioFeedback descriptionMode="tooltip" grouped={true} />
        <OutputDeviceSelector
          descriptionMode="tooltip"
          grouped={true}
          disabled={!audioFeedbackEnabled}
        />
        <VolumeSlider disabled={!audioFeedbackEnabled} />
      </SettingsGroup>
    </div>
  );
};
