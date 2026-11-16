import React from "react";
import { useTranslation } from "react-i18next";
import { SettingsGroup } from "../../ui/SettingsGroup";
import { VoiceInsertApi } from "../VoiceInsertApi";

export const IntegrationsSettings: React.FC = () => {
  const { t } = useTranslation();

  return (
    <div className="max-w-3xl w-full mx-auto space-y-6">
      <SettingsGroup title={t("settings.integrations.title")}>
        <VoiceInsertApi descriptionMode="inline" grouped />
      </SettingsGroup>
    </div>
  );
};
