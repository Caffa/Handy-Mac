import React, { useState, useEffect } from "react";
import { useTranslation } from "react-i18next";
import { commands } from "../../../bindings";
import { SettingsGroup } from "../../ui/SettingsGroup";

const LEVEL_DESCRIPTIONS = {
  0: "Off - No repetition suppression",
  1: "Light - Remove 3+ consecutive repetitions",
  2: "Moderate - Remove 2+ consecutive repetitions",
  3: "Aggressive - Same as Moderate",
};

interface AppSettingsWithRepetition {
  repetition_suppression_level?: number;
}

export const RepetitionSuppressionSettings: React.FC = () => {
  const { t } = useTranslation();
  const [level, setLevel] = useState<number>(0);
  const [loading, setLoading] = useState(true);

  useEffect(() => {
    loadLevel();
  }, []);

  const loadLevel = async () => {
    try {
      const settings = await commands.getAppSettings();
      if (settings.status === "ok") {
        // Access the field via type assertion since bindings may not be regenerated yet
        const settingsData =
          settings.data as unknown as AppSettingsWithRepetition;
        setLevel(settingsData.repetition_suppression_level ?? 0);
      }
    } catch (e) {
      console.error("Failed to load repetition suppression level:", e);
    } finally {
      setLoading(false);
    }
  };

  const handleLevelChange = async (newLevel: number) => {
    await commands.setRepetitionSuppressionLevel(newLevel);
    setLevel(newLevel);
  };

  if (loading) {
    return (
      <div className="text-sm text-gray-400">
        {t("settings.debug.repetitionSuppression.loading")}
      </div>
    );
  }

  return (
    <SettingsGroup
      title={t("settings.debug.repetitionSuppression.title")}
      description={t("settings.debug.repetitionSuppression.description")}
    >
      <div className="space-y-4">
        {/* Level Selection */}
        <div className="flex items-center justify-between">
          <div>
            <div className="text-sm font-medium">
              {t("settings.debug.repetitionSuppression.level")}
            </div>
            <div className="text-xs text-gray-400">
              {LEVEL_DESCRIPTIONS[level as keyof typeof LEVEL_DESCRIPTIONS]}
            </div>
          </div>
          <select
            value={level}
            onChange={(e) => handleLevelChange(parseInt(e.target.value))}
            className="px-3 py-1.5 bg-gray-700 border border-gray-600 rounded-md text-sm focus:outline-none focus:ring-2 focus:ring-blue-500"
          >
            <option value={0}>Off</option>
            <option value={1}>Light</option>
            <option value={2}>Moderate</option>
            <option value={3}>Aggressive</option>
          </select>
        </div>

        {/* Protected Words Notice */}
        <div className="mt-4 p-3 bg-blue-900/20 border border-blue-700/30 rounded-md">
          <div className="text-xs text-blue-300">
            {t("settings.debug.repetitionSuppression.protectedNotice")}
          </div>
        </div>
      </div>
    </SettingsGroup>
  );
};
