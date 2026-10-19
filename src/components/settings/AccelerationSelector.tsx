import { type FC, useEffect, useState } from "react";
import { useTranslation } from "react-i18next";
import { SettingContainer } from "../ui/SettingContainer";
import { Dropdown, type DropdownOption } from "../ui/Dropdown";
import { useSettings } from "../../hooks/useSettings";
import { commands } from "@/bindings";
import type { TranscribeAcceleratorSetting } from "@/bindings";

interface AccelerationSelectorProps {
  descriptionMode?: "tooltip" | "inline";
  grouped?: boolean;
}

/**
 * transcribe.cpp dropdown encodes accelerator + device in a single value:
 *   "auto"       → accelerator=auto, gpu_device=null
 *   "cpu"        → accelerator=cpu,  gpu_device=null
 *   "gpu:<id>"   → accelerator=gpu, stable opaque device identity
 */
function encodeTranscribeValue(
  accelerator: TranscribeAcceleratorSetting,
  gpuDevice: string | null,
): string {
  if (accelerator === "cpu") return "cpu";
  if (accelerator === "gpu" && gpuDevice !== null) return `gpu:${gpuDevice}`;
  return "auto";
}

function decodeTranscribeValue(value: string): {
  accelerator: TranscribeAcceleratorSetting;
  gpuDevice: string | null;
} {
  if (value === "cpu") return { accelerator: "cpu", gpuDevice: null };
  if (value.startsWith("gpu:")) {
    return { accelerator: "gpu", gpuDevice: value.slice(4) };
  }
  return { accelerator: "auto", gpuDevice: null };
}

export const AccelerationSelector: FC<AccelerationSelectorProps> = ({
  descriptionMode = "tooltip",
  grouped = false,
}) => {
  const { t } = useTranslation();
  const { getSetting, updateSetting, isUpdating } = useSettings();

  const [whisperOptions, setWhisperOptions] = useState<DropdownOption[]>([]);

  useEffect(() => {
    commands.getAvailableAccelerators().then((result) => {
      if (result.status !== "ok") {
        console.error("Failed to get available accelerators:", result.error);
        return;
      }
      const available = result.data;
      // Build combined Whisper options: Auto, [GPU devices...], CPU
      const opts: DropdownOption[] = [
        {
          value: "auto",
          label: t("settings.advanced.acceleration.gpuDevice.auto"),
        },
      ];

      for (const dev of available.gpu_devices) {
        const vramLabel =
          dev.total_vram_mb >= 1024
            ? `${(dev.total_vram_mb / 1024).toFixed(1)} GB`
            : `${dev.total_vram_mb} MB`;
        opts.push({
          value: `gpu:${dev.id}`,
          label: `${dev.name} (${vramLabel})`,
        });
      }

      opts.push({ value: "cpu", label: "CPU" });
      setWhisperOptions(opts);
    });
  }, [t]);

  const currentAccelerator = getSetting("transcribe_accelerator") ?? "auto";
  const currentGpuDevice = getSetting("transcribe_gpu_device") ?? null;
  const currentTranscribe = encodeTranscribeValue(
    currentAccelerator as TranscribeAcceleratorSetting,
    currentGpuDevice as string | null,
  );

  const handleTranscribeChange = async (value: string) => {
    const { accelerator, gpuDevice } = decodeTranscribeValue(value);
    // Save the device first to avoid `gpu + null` being normalized to Auto.
    await updateSetting("transcribe_gpu_device", gpuDevice);
    await updateSetting("transcribe_accelerator", accelerator);
  };

  return (
    <SettingContainer
      title={t("settings.advanced.acceleration.whisper.title")}
      description={t("settings.advanced.acceleration.whisper.description")}
      descriptionMode={descriptionMode}
      grouped={grouped}
      layout="horizontal"
    >
      <Dropdown
        options={whisperOptions}
        selectedValue={currentTranscribe}
        onSelect={handleTranscribeChange}
        disabled={
          isUpdating("transcribe_accelerator") ||
          isUpdating("transcribe_gpu_device")
        }
      />
    </SettingContainer>
  );
};
