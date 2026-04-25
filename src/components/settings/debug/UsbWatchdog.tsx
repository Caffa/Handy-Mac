import React, { useState } from "react";
import { useTranslation } from "react-i18next";
import { ToggleSwitch } from "../../ui/ToggleSwitch";
import { useSettings } from "../../../hooks/useSettings";
import { commands } from "@/bindings";

interface UsbWatchdogProps {
  descriptionMode?: "inline" | "tooltip";
  grouped?: boolean;
}

export const UsbWatchdog: React.FC<UsbWatchdogProps> = React.memo(
  ({ descriptionMode = "tooltip", grouped = false }) => {
    const { t } = useTranslation();
    const { getSetting, updateSetting, isUpdating } = useSettings();

    const enabled = getSetting("usb_watchdog_enabled") ?? false;
    const hubId = getSetting("usb_watchdog_hub_id") ?? "8-3";
    const port = getSetting("usb_watchdog_port") ?? "2";
    const [cycling, setCycling] = useState(false);

    const handleCycle = async () => {
      setCycling(true);
      try {
        await commands.triggerUsbPowerCycle();
      } catch (e) {
        console.error("USB power cycle failed:", e);
      } finally {
        // Give time for uhubctl to finish
        setTimeout(() => setCycling(false), 15000);
      }
    };

    // Only show on macOS where uhubctl is available
    const available = commands.isUsbWatchdogAvailable();

    if (!available) {
      return null;
    }

    return (
      <div className="space-y-3">
        <ToggleSwitch
          checked={enabled}
          onChange={(val: boolean) => updateSetting("usb_watchdog_enabled", val)}
          isUpdating={isUpdating("usb_watchdog_enabled")}
          label={t("settings.debug.usbWatchdog.label")}
          description={t("settings.debug.usbWatchdog.description")}
          descriptionMode={descriptionMode}
          grouped={grouped}
        />
        {enabled && (
          <div className="pl-4 space-y-3 border-l-2 border-gray-200 dark:border-gray-700 ml-1">
            <div className="flex flex-col space-y-1">
              <label className="text-sm font-medium text-gray-700 dark:text-gray-300">
                {t("settings.debug.usbWatchdog.hubId")}
              </label>
              <input
                type="text"
                value={hubId ?? ""}
                onChange={(e) =>
                  updateSetting("usb_watchdog_hub_id", e.target.value)
                }
                placeholder="8-3"
                className="px-3 py-1.5 text-sm rounded-md border border-gray-300 dark:border-gray-600 bg-white dark:bg-gray-800 text-gray-900 dark:text-gray-100 focus:ring-2 focus:ring-blue-500 focus:border-blue-500 w-32"
              />
            </div>
            <div className="flex flex-col space-y-1">
              <label className="text-sm font-medium text-gray-700 dark:text-gray-300">
                {t("settings.debug.usbWatchdog.port")}
              </label>
              <input
                type="text"
                value={port ?? ""}
                onChange={(e) =>
                  updateSetting("usb_watchdog_port", e.target.value)
                }
                placeholder="2"
                className="px-3 py-1.5 text-sm rounded-md border border-gray-300 dark:border-gray-600 bg-white dark:bg-gray-800 text-gray-900 dark:text-gray-100 focus:ring-2 focus:ring-blue-500 focus:border-blue-500 w-20"
              />
            </div>
            <button
              onClick={handleCycle}
              disabled={cycling}
              className="px-3 py-1.5 text-sm rounded-md bg-yellow-600 hover:bg-yellow-700 text-white disabled:opacity-50 disabled:cursor-not-allowed transition-colors"
            >
              {cycling
                ? t("settings.debug.usbWatchdog.cycling")
                : t("settings.debug.usbWatchdog.testCycle")}
            </button>
          </div>
        )}
      </div>
    );
  },
);