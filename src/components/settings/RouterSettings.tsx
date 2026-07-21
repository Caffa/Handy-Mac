import React from "react";
import { useTranslation } from "react-i18next";
import { ToggleSwitch } from "../ui/ToggleSwitch";
import { Input } from "../ui/Input";
import { useSettings } from "../../hooks/useSettings";

interface RouterSettingsProps {
  descriptionMode?: "inline" | "tooltip";
  grouped?: boolean;
}

export const RouterSettings: React.FC<RouterSettingsProps> = React.memo(
  ({ descriptionMode = "tooltip", grouped = false }) => {
    const { t } = useTranslation();
    const { getSetting, updateSetting, isUpdating } = useSettings();

    const enabled = getSetting("router_mode_enabled") ?? false;
    const scriptPath = getSetting("router_script_path") ?? "";
    const envFile = getSetting("router_env_file") ?? "";

    return (
      <>
        <ToggleSwitch
          checked={enabled}
          onChange={(val) => updateSetting("router_mode_enabled", val)}
          isUpdating={isUpdating("router_mode_enabled")}
          label={t("settings.advanced.routerMode.label")}
          description={t("settings.advanced.routerMode.description")}
          descriptionMode={descriptionMode}
          grouped={grouped}
        />

        {enabled && (
          <div className="mt-4 ml-2 border-l-2 border-logo-primary/20 pl-4 space-y-4">
            <div>
              <label className="block text-xs font-medium text-text/70 mb-1">
                {t("settings.advanced.routerMode.scriptPathLabel")}
              </label>
              <Input
                type="text"
                value={scriptPath}
                onChange={(e) =>
                  updateSetting("router_script_path", e.target.value || null)
                }
                placeholder={t("settings.advanced.routerMode.scriptPathPlaceholder")}
                className="w-full"
              />
              <p className="mt-1 text-xs text-text/50">
                {t("settings.advanced.routerMode.scriptPathDescription")}
              </p>
            </div>

            <div>
              <label className="block text-xs font-medium text-text/70 mb-1">
                {t("settings.advanced.routerMode.envFileLabel")}
              </label>
              <Input
                type="text"
                value={envFile}
                onChange={(e) =>
                  updateSetting("router_env_file", e.target.value || null)
                }
                placeholder={t("settings.advanced.routerMode.envFilePlaceholder")}
                className="w-full"
              />
              <p className="mt-1 text-xs text-text/50">
                {t("settings.advanced.routerMode.envFileDescription")}
              </p>
            </div>
          </div>
        )}
      </>
    );
  },
);

RouterSettings.displayName = "RouterSettings";
