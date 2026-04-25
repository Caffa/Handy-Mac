import React, { useState, useEffect, useCallback } from "react";
import { useTranslation } from "react-i18next";
import { ToggleSwitch } from "../../ui/ToggleSwitch";
import { useSettings } from "../../../hooks/useSettings";
import { commands, type UsbDevice } from "@/bindings";

interface UsbWatchdogProps {
  descriptionMode?: "inline" | "tooltip";
  grouped?: boolean;
}

export const UsbWatchdog: React.FC<UsbWatchdogProps> = React.memo(
  ({ descriptionMode = "tooltip", grouped = false }) => {
    const { t } = useTranslation();
    const { getSetting, updateSetting, isUpdating } = useSettings();

    const enabled = getSetting("usb_watchdog_enabled") ?? false;
    const deviceName = getSetting("usb_watchdog_device_name") ?? "";
    const [devices, setDevices] = useState<UsbDevice[]>([]);
    const [cycling, setCycling] = useState(false);
    const [loading, setLoading] = useState(false);
    const [available, setAvailable] = useState(false);

    // Check uhubctl availability on mount
    useEffect(() => {
      commands.isUsbWatchdogAvailable().then(setAvailable);
    }, []);

    const refreshDevices = useCallback(async () => {
      setLoading(true);
      try {
        const result = await commands.listUsbDevices();
        if (result.status === "ok") {
          setDevices(result.data);
        }
      } catch {
        // uhubctl not available, list will be empty
      } finally {
        setLoading(false);
      }
    }, []);

    useEffect(() => {
      if (available && enabled) {
        refreshDevices();
      }
    }, [available, enabled, refreshDevices]);

    const handleCycle = async () => {
      setCycling(true);
      try {
        await commands.triggerUsbPowerCycle();
      } catch (e) {
        console.error("USB power cycle failed:", e);
      } finally {
        setTimeout(() => setCycling(false), 15000);
      }
    };

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
                {t("settings.debug.usbWatchdog.device")}
              </label>
              <div className="flex items-center gap-2">
                <select
                  value={deviceName ?? ""}
                  onChange={(e) =>
                    updateSetting("usb_watchdog_device_name", e.target.value)
                  }
                  className="flex-1 px-3 py-1.5 text-sm rounded-md border border-gray-300 dark:border-gray-600 bg-white dark:bg-gray-800 text-gray-900 dark:text-gray-100 focus:ring-2 focus:ring-blue-500 focus:border-blue-500"
                >
                  <option value="">
                    {loading
                      ? t("settings.debug.usbWatchdog.loading")
                      : t("settings.debug.usbWatchdog.selectDevice")}
                  </option>
                  {devices.map((device) => (
                    <option key={`${device.hub}-${device.port}`} value={device.name}>
                      {device.name}
                    </option>
                  ))}
                </select>
                <button
                  onClick={refreshDevices}
                  disabled={loading}
                  className="px-2 py-1.5 text-sm rounded-md border border-gray-300 dark:border-gray-600 bg-white dark:bg-gray-800 text-gray-700 dark:text-gray-300 hover:bg-gray-50 dark:hover:bg-gray-700 disabled:opacity-50"
                  title={t("settings.debug.usbWatchdog.refreshDevices")}
                >
                  ↻
                </button>
              </div>
              {deviceName && (
                <p className="text-xs text-gray-500 dark:text-gray-400">
                  {devices.find((d) => d.name === deviceName)
                    ? `Hub ${devices.find((d) => d.name === deviceName)!.hub}, Port ${devices.find((d) => d.name === deviceName)!.port}`
                    : t("settings.debug.usbWatchdog.deviceNotFound")}
                </p>
              )}
            </div>
            <button
              onClick={handleCycle}
              disabled={cycling || !deviceName}
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