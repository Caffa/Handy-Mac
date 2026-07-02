import React from "react";
import { useTranslation } from "react-i18next";
import { useSettings } from "../../hooks/useSettings";

interface OverlayScaleProps {
  descriptionMode?: "inline" | "tooltip";
  grouped?: boolean;
}

export const OverlayScale: React.FC<OverlayScaleProps> = React.memo(
  ({ descriptionMode = "tooltip", grouped = false }) => {
    const { t } = useTranslation();
    const { getSetting, updateSetting, isUpdating } = useSettings();
    const scale = getSetting("overlay_scale") ?? 1.0;

    const options = [
      {
        value: 1.0,
        label: t("settings.advanced.overlayScale.options.normal", "Normal"),
      },
      {
        value: 2.0,
        label: t("settings.advanced.overlayScale.options.large", "Large (2x)"),
      },
    ];

    const selectedOption =
      options.find((opt) => opt.value === scale) || options[0];

    return (
      <div className={`settings-item ${grouped ? "grouped" : ""}`}>
        <div className="settings-item-content">
          <div className="settings-item-header">
            <span className="settings-item-label">
              {t("settings.advanced.overlayScale.label", "Overlay Size")}
            </span>
            {descriptionMode === "tooltip" && (
              <span
                className="settings-item-description"
                title={t(
                  "settings.advanced.overlayScale.description",
                  "Scale the overlay pill and live captions. Large mode makes text easier to read.",
                )}
              >
                {t(
                  "settings.advanced.overlayScale.description",
                  "Scale the overlay pill and live captions. Large mode makes text easier to read.",
                )}
              </span>
            )}
          </div>
          {descriptionMode === "inline" && (
            <div className="settings-item-description-inline">
              {t(
                "settings.advanced.overlayScale.description",
                "Scale the overlay pill and live captions. Large mode makes text easier to read.",
              )}
            </div>
          )}
        </div>
        <div className="settings-item-control">
          <select
            value={selectedOption.value}
            onChange={(e) =>
              updateSetting("overlay_scale", parseFloat(e.target.value))
            }
            disabled={isUpdating("overlay_scale")}
            className="settings-dropdown"
          >
            {options.map((opt) => (
              <option key={opt.value} value={opt.value}>
                {opt.label}
              </option>
            ))}
          </select>
        </div>
      </div>
    );
  },
);
