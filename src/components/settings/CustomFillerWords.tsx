import React, { useState } from "react";
import { useTranslation } from "react-i18next";
import { toast } from "sonner";
import { useSettings } from "../../hooks/useSettings";
import { Input } from "../ui/Input";
import { Button } from "../ui/Button";
import { SettingContainer } from "../ui/SettingContainer";

interface CustomFillerWordsProps {
  descriptionMode?: "inline" | "tooltip";
  grouped?: boolean;
}

export const CustomFillerWords: React.FC<CustomFillerWordsProps> = React.memo(
  ({ descriptionMode = "tooltip", grouped = false }) => {
    const { t } = useTranslation();
    const { getSetting, updateSetting, isUpdating } = useSettings();
    const [newWord, setNewWord] = useState("");
    const customFillerWords = getSetting("custom_filler_words") || [];

    const handleAddWord = () => {
      const trimmedWord = newWord.trim().toLowerCase();
      const sanitizedWord = trimmedWord.replace(/[<>"'&]/g, "");
      if (
        sanitizedWord &&
        !sanitizedWord.includes(" ") &&
        sanitizedWord.length <= 30
      ) {
        if (customFillerWords.includes(sanitizedWord)) {
          toast.error(
            t("settings.advanced.customFillerWords.duplicate", {
              word: sanitizedWord,
            }),
          );
          return;
        }
        updateSetting("custom_filler_words", [
          ...customFillerWords,
          sanitizedWord,
        ]);
        setNewWord("");
      }
    };

    const handleRemoveWord = (wordToRemove: string) => {
      updateSetting(
        "custom_filler_words",
        customFillerWords.filter((word: string) => word !== wordToRemove),
      );
    };

    const handleKeyPress = (e: React.KeyboardEvent) => {
      if (e.key === "Enter") {
        e.preventDefault();
        handleAddWord();
      }
    };

    return (
      <>
        <SettingContainer
          title={t("settings.advanced.customFillerWords.title")}
          description={t("settings.advanced.customFillerWords.description")}
          descriptionMode={descriptionMode}
          grouped={grouped}
        >
          <div className="flex items-center gap-2">
            <Input
              type="text"
              className="max-w-40"
              value={newWord}
              onChange={(e) => setNewWord(e.target.value)}
              onKeyDown={handleKeyPress}
              placeholder={t("settings.advanced.customFillerWords.placeholder")}
              variant="compact"
              disabled={isUpdating("custom_filler_words")}
            />
            <Button
              onClick={handleAddWord}
              disabled={
                !newWord.trim() ||
                newWord.includes(" ") ||
                newWord.trim().length > 30 ||
                isUpdating("custom_filler_words")
              }
              variant="primary"
              size="md"
            >
              {t("settings.advanced.customFillerWords.add")}
            </Button>
          </div>
        </SettingContainer>
        {customFillerWords.length > 0 && (
          <div
            className={`px-4 p-2 ${grouped ? "" : "rounded-lg border border-mid-gray/20"} flex flex-wrap gap-1`}
          >
            {customFillerWords.map((word: string) => (
              <Button
                key={word}
                onClick={() => handleRemoveWord(word)}
                disabled={isUpdating("custom_filler_words")}
                variant="secondary"
                size="sm"
                className="inline-flex items-center gap-1 cursor-pointer"
                aria-label={t("settings.advanced.customFillerWords.remove", {
                  word,
                })}
              >
                <span>{word}</span>
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
            ))}
          </div>
        )}
      </>
    );
  },
);

CustomFillerWords.displayName = "CustomFillerWords";