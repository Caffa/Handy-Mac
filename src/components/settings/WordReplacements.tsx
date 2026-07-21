import React, { useState } from "react";
import { useTranslation } from "react-i18next";
import { toast } from "sonner";
import { useSettings } from "../../hooks/useSettings";
import type { WordReplacement } from "@/bindings";
import { Input } from "../ui/Input";
import { Button } from "../ui/Button";
import { SettingContainer } from "../ui/SettingContainer";

interface WordReplacementsProps {
  descriptionMode?: "inline" | "tooltip";
  grouped?: boolean;
}

export const WordReplacements: React.FC<WordReplacementsProps> = React.memo(
  ({ descriptionMode = "tooltip", grouped = false }) => {
    const { t } = useTranslation();
    const { getSetting, updateSetting, isUpdating } = useSettings();
    const wordReplacements =
      (getSetting("word_replacements") as WordReplacement[] | undefined) || [];
    const [newMistranslation, setNewMistranslation] = useState("");
    const [newCorrection, setNewCorrection] = useState("");

    const handleAddReplacement = () => {
      const trimmedMistranslation = newMistranslation.trim();
      const trimmedCorrection = newCorrection.trim();

      // Sanitize inputs
      const sanitizedMistranslation = trimmedMistranslation.replace(
        /[<>"'&]/g,
        "",
      );
      const sanitizedCorrection = trimmedCorrection.replace(/[<>"'&]/g, "");

      if (!sanitizedMistranslation || !sanitizedCorrection) {
        toast.error(t("settings.debug.wordReplacements.bothFieldsRequired"));
        return;
      }

      if (sanitizedMistranslation.length > 100) {
        toast.error(t("settings.debug.wordReplacements.mistranslationTooLong"));
        return;
      }

      if (sanitizedCorrection.length > 100) {
        toast.error(t("settings.debug.wordReplacements.correctionTooLong"));
        return;
      }

      // Check for duplicate mistranslation
      if (
        wordReplacements.some(
          (r: WordReplacement) =>
            r.mistranslation.toLowerCase() ===
            sanitizedMistranslation.toLowerCase(),
        )
      ) {
        toast.error(
          t("settings.debug.wordReplacements.duplicate", {
            mistranslation: sanitizedMistranslation,
          }),
        );
        return;
      }

      const newEntry = {
        mistranslation: sanitizedMistranslation,
        correction: sanitizedCorrection,
      };
      updateSetting("word_replacements", [...wordReplacements, newEntry]);
      setNewMistranslation("");
      setNewCorrection("");
    };

    const handleRemoveReplacement = (index: number) => {
      const updated = wordReplacements.filter(
        (_: WordReplacement, i: number) => i !== index,
      );
      updateSetting("word_replacements", updated);
    };

    const handleKeyPress = (e: React.KeyboardEvent) => {
      if (e.key === "Enter") {
        e.preventDefault();
        handleAddReplacement();
      }
    };

    return (
      <>
        <SettingContainer
          title={t("settings.debug.wordReplacements.title")}
          description={t("settings.debug.wordReplacements.description")}
          descriptionMode={descriptionMode}
          grouped={grouped}
        >
          <div className="flex flex-col gap-2">
            <div className="text-xs text-text-secondary">
              {t("settings.debug.wordReplacements.hint")}
            </div>
            <div className="flex items-end gap-2">
              <div className="flex flex-col gap-1">
                <Input
                  type="text"
                  className="max-w-48"
                  value={newMistranslation}
                  onChange={(e) => setNewMistranslation(e.target.value)}
                  onKeyDown={handleKeyPress}
                  placeholder={t(
                    "settings.debug.wordReplacements.mistranslationPlaceholder",
                  )}
                  variant="compact"
                  disabled={isUpdating("word_replacements")}
                />
              </div>
              <div className="flex flex-col gap-1">
                <Input
                  type="text"
                  className="max-w-48"
                  value={newCorrection}
                  onChange={(e) => setNewCorrection(e.target.value)}
                  onKeyDown={handleKeyPress}
                  placeholder={t(
                    "settings.debug.wordReplacements.correctionPlaceholder",
                  )}
                  variant="compact"
                  disabled={isUpdating("word_replacements")}
                />
              </div>
              <Button
                onClick={handleAddReplacement}
                disabled={
                  !newMistranslation.trim() ||
                  !newCorrection.trim() ||
                  isUpdating("word_replacements")
                }
                variant="primary"
                size="md"
              >
                {t("settings.debug.wordReplacements.add")}
              </Button>
            </div>
          </div>
        </SettingContainer>
        {wordReplacements.length > 0 && (
          <div
            className={`px-4 p-2 ${grouped ? "" : "rounded-lg border border-mid-gray/20"} space-y-1`}
          >
            {wordReplacements.map(
              (replacement: WordReplacement, index: number) => (
                <div
                  key={`${replacement.mistranslation}-${index}`}
                  className="flex items-center gap-2"
                >
                  <Button
                    onClick={() => handleRemoveReplacement(index)}
                    disabled={isUpdating("word_replacements")}
                    variant="secondary"
                    size="sm"
                    className="inline-flex items-center gap-1 cursor-pointer"
                    aria-label={t("settings.debug.wordReplacements.remove", {
                      mistranslation: replacement.mistranslation,
                      correction: replacement.correction,
                    })}
                  >
                    <span className="text-text-secondary">
                      {replacement.mistranslation}
                    </span>
                    <span className="text-text-primary">→</span>
                    <span className="text-text-primary font-medium">
                      {replacement.correction}
                    </span>
                    <svg
                      className="w-3 h-3"
                      fill="none"
                      stroke="currentColor"
                      viewBox="0 0 24 24"
                    >
                      <path
                        strokeLinecap="round"
                        strokeLinejoin="round"
                        strokeWidth={2}
                        d="M6 18L18 6M6 6l12 12"
                      />
                    </svg>
                  </Button>
                </div>
              ),
            )}
          </div>
        )}
      </>
    );
  },
);

WordReplacements.displayName = "WordReplacements";
