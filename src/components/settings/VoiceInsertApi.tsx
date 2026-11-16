import React, { useState } from "react";
import { useTranslation } from "react-i18next";
import { ToggleSwitch } from "../ui/ToggleSwitch";
import { SettingContainer } from "../ui/SettingContainer";
import { Button } from "../ui/Button";
import { Input } from "../ui/Input";
import { useSettings } from "../../hooks/useSettings";
import { commands } from "@/bindings";

interface VoiceInsertApiProps {
  descriptionMode?: "inline" | "tooltip";
  grouped?: boolean;
}

const DEFAULT_VOICE_INSERT_PORT = 48213;

export const VoiceInsertApi: React.FC<VoiceInsertApiProps> = React.memo(
  ({ descriptionMode = "tooltip", grouped = false }) => {
    const { t } = useTranslation();
    const { getSetting, updateSetting, refreshSettings } = useSettings();
    const enabled = getSetting("voice_insert_enabled") ?? false;
    const port = getSetting("voice_insert_port") ?? DEFAULT_VOICE_INSERT_PORT;
    const token = getSetting("voice_insert_token") ?? "";

    const [copied, setCopied] = useState(false);
    const [regenerating, setRegenerating] = useState(false);
    const [portDraft, setPortDraft] = useState<string | null>(null);

    const handleToggleChange = (checked: boolean) => {
      updateSetting("voice_insert_enabled", checked);
    };

    const handlePortCommit = () => {
      if (portDraft === null) return;
      const parsed = Number(portDraft);
      if (
        Number.isInteger(parsed) &&
        parsed > 0 &&
        parsed <= 65535 &&
        parsed !== port
      ) {
        updateSetting("voice_insert_port", parsed);
      }
      setPortDraft(null);
    };

    const handleCopy = async () => {
      try {
        await navigator.clipboard.writeText(token);
        setCopied(true);
        setTimeout(() => setCopied(false), 1500);
      } catch (err) {
        console.error("Failed to copy voice insert token:", err);
      }
    };

    const handleRegenerate = async () => {
      const { toast } = await import("sonner");
      setRegenerating(true);
      try {
        const result = await commands.regenerateVoiceInsertToken();
        if (result.status === "ok") {
          await refreshSettings();
        } else {
          toast.error(t("errors.settingsLoadFailed"), {
            description: result.error,
          });
        }
      } catch (err) {
        toast.error(t("errors.settingsLoadFailed"), {
          description: String(err),
        });
      } finally {
        setRegenerating(false);
      }
    };

    return (
      <div className="space-y-2">
        <ToggleSwitch
          checked={enabled}
          onChange={handleToggleChange}
          label={t("settings.voiceInsert.label")}
          description={t("settings.voiceInsert.description")}
          descriptionMode={descriptionMode}
          grouped={grouped}
        />
        {enabled && (
          <>
            <SettingContainer
              title={t("settings.voiceInsert.portTitle")}
              description={t("settings.voiceInsert.portDescription")}
              descriptionMode={descriptionMode}
              grouped={grouped}
              layout="horizontal"
            >
              <Input
                type="number"
                min={1}
                max={65535}
                value={portDraft ?? String(port)}
                onChange={(e) => setPortDraft(e.target.value)}
                onBlur={handlePortCommit}
                className="w-28"
              />
            </SettingContainer>
            <SettingContainer
              title={t("settings.voiceInsert.tokenTitle")}
              description={t("settings.voiceInsert.tokenDescription")}
              descriptionMode={descriptionMode}
              grouped={grouped}
              layout="horizontal"
            >
              <div className="flex items-center gap-2">
                <span
                  className="font-mono text-xs text-mid-gray max-w-40 truncate"
                  title={token || undefined}
                >
                  {token || t("settings.voiceInsert.tokenEmpty")}
                </span>
                <Button
                  variant="secondary"
                  size="sm"
                  onClick={handleCopy}
                  disabled={!token}
                >
                  {copied
                    ? t("settings.voiceInsert.copied")
                    : t("settings.voiceInsert.copy")}
                </Button>
                <Button
                  variant="secondary"
                  size="sm"
                  onClick={handleRegenerate}
                  disabled={regenerating}
                >
                  {t("settings.voiceInsert.regenerate")}
                </Button>
              </div>
            </SettingContainer>
            <p className="px-4 text-xs text-mid-gray">
              {t("settings.voiceInsert.restartHint")}
            </p>
          </>
        )}
      </div>
    );
  },
);
