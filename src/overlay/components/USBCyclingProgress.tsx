/**
 * USBCyclingProgress — Presentational component for USB cycling progress.
 *
 * Shows the current USB recovery stage, progress dots, and elapsed time.
 *
 * Scope: Pure presentational — no state or side effects.
 */
import React from "react";
import { useTranslation } from "react-i18next";

interface USBCyclingProgressProps {
  usbCycleStage: { stage: string; message: string } | null;
  usbCyclingElapsed: number;
}

const STAGE_ORDER = ["resolving", "cycling", "waiting", "recovered"];

export function USBCyclingProgress({
  usbCycleStage,
  usbCyclingElapsed,
}: USBCyclingProgressProps) {
  const { t } = useTranslation();

  return (
    <div className="usb-cycling-container">
      <div className="usb-cycling-stage">
        {usbCycleStage
          ? usbCycleStage.message
          : t("overlay.usbCycling", "USB cycling…")}
      </div>
      {usbCycleStage && (
        <>
          <div className="usb-cycling-progress">
            {STAGE_ORDER.map((s) => (
              <div
                key={s}
                className={`usb-cycling-dot ${
                  STAGE_ORDER.indexOf(usbCycleStage.stage) >=
                  STAGE_ORDER.indexOf(s)
                    ? "dot-active"
                    : ""
                } ${usbCycleStage.stage === s ? "dot-current" : ""}`}
              />
            ))}
          </div>
          {usbCyclingElapsed > 0 && (
            <div className="usb-cycling-time">
              {t("overlay.usbCyclingTime", { seconds: usbCyclingElapsed })}
            </div>
          )}
        </>
      )}
    </div>
  );
}
