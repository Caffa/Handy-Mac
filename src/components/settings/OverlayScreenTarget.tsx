import React, { useEffect, useState } from "react";
import { useTranslation } from "react-i18next";
import { Dropdown } from "../ui/Dropdown";
import { SettingContainer } from "../ui/SettingContainer";
import { useSettings } from "../../hooks/useSettings";
import { availableMonitors } from "@tauri-apps/api/window";
import type { OverlayScreenTarget as OverlayScreenTargetType } from "@/bindings";

interface OverlayScreenTargetProps {
  descriptionMode?: "inline" | "tooltip";
  grouped?: boolean;
}

export const OverlayScreenTarget: React.FC<OverlayScreenTargetProps> = React.memo(
  ({ descriptionMode = "tooltip", grouped = false }) => {
    const { t } = useTranslation();
    const { getSetting, updateSetting, isUpdating } = useSettings();
    const [monitorCount, setMonitorCount] = useState<number | null>(null);

    useEffect(() => {
      availableMonitors()
        .then((monitors) => setMonitorCount(monitors.length))
        .catch(() => setMonitorCount(1));
    }, []);

    // Hide the setting when there's only one monitor (or while loading)
    if (monitorCount === null || monitorCount < 2) {
      return null;
    }

    const options = [
      { value: "cursor", label: t("settings.advanced.overlayScreenTarget.options.cursor") },
      { value: "side_screen", label: t("settings.advanced.overlayScreenTarget.options.sideScreen") },
    ];

    const selectedTarget = (getSetting("overlay_screen_target") ||
      "cursor") as OverlayScreenTargetType;

    return (
      <SettingContainer
        title={t("settings.advanced.overlayScreenTarget.title")}
        description={t("settings.advanced.overlayScreenTarget.description")}
        descriptionMode={descriptionMode}
        grouped={grouped}
      >
        <Dropdown
          options={options}
          selectedValue={selectedTarget}
          onSelect={(value) =>
            updateSetting("overlay_screen_target", value as OverlayScreenTargetType)
          }
          disabled={isUpdating("overlay_screen_target")}
        />
      </SettingContainer>
    );
  },
);