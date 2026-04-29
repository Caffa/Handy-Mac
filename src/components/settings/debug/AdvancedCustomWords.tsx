import React, { useState } from "react";
import { useTranslation } from "react-i18next";
import { toast } from "sonner";
import { useSettings } from "../../../hooks/useSettings";
import type { CustomWord } from "../../../bindings";
import { Input } from "../../ui/Input";
import { Button } from "../../ui/Button";
import { SettingContainer } from "../../ui/SettingContainer";

interface AdvancedCustomWordsProps {
  descriptionMode?: "inline" | "tooltip";
  grouped?: boolean;
}

export const AdvancedCustomWords: React.FC<AdvancedCustomWordsProps> = React.memo(
  ({ descriptionMode = "tooltip", grouped = false }) => {
    const { t } = useTranslation();
    const { getSetting, updateSetting, isUpdating } = useSettings();
    const advancedWords = getSetting("advanced_custom_words") || [];

    const [newWord, setNewWord] = useState("");
    const [expandedIndex, setExpandedIndex] = useState<number | null>(null);
    const [newPronunciation, setNewPronunciation] = useState("");

    const handleAddWord = () => {
      const trimmedWord = newWord.trim();
      const sanitizedWord = trimmedWord.replace(/[<>"'&]/g, "");
      if (!sanitizedWord || sanitizedWord.length > 100) return;

      if (advancedWords.some((w: CustomWord) => w.word === sanitizedWord)) {
        toast.error(
          t("settings.debug.advancedCustomWords.duplicate", {
            word: sanitizedWord,
          }),
        );
        return;
      }

      const newEntry: CustomWord = {
        word: sanitizedWord,
        pronunciations: [],
      };
      updateSetting("advanced_custom_words", [...advancedWords, newEntry]);
      setNewWord("");
    };

    const handleRemoveWord = (index: number) => {
      const updated = advancedWords.filter((_: CustomWord, i: number) => i !== index);
      updateSetting("advanced_custom_words", updated);
      if (expandedIndex === index) {
        setExpandedIndex(null);
      } else if (expandedIndex !== null && expandedIndex > index) {
        setExpandedIndex(expandedIndex - 1);
      }
    };

    const handleAddPronunciation = (wordIndex: number) => {
      const trimmed = newPronunciation.trim();
      if (!trimmed) return;

      const updated = [...advancedWords];
      const word = updated[wordIndex];
      if (word.pronunciations.includes(trimmed)) {
        toast.error(
          t("settings.debug.advancedCustomWords.duplicatePronunciation", {
            pronunciation: trimmed,
          }),
        );
        return;
      }
      updated[wordIndex] = {
        ...word,
        pronunciations: [...word.pronunciations, trimmed],
      };
      updateSetting("advanced_custom_words", updated);
      setNewPronunciation("");
    };

    const handleRemovePronunciation = (wordIndex: number, pronIndex: number) => {
      const updated = [...advancedWords];
      const word = updated[wordIndex];
      updated[wordIndex] = {
        ...word,
        pronunciations: word.pronunciations.filter((_: string, i: number) => i !== pronIndex),
      };
      updateSetting("advanced_custom_words", updated);
    };

    const handleKeyPressWord = (e: React.KeyboardEvent) => {
      if (e.key === "Enter") {
        e.preventDefault();
        handleAddWord();
      }
    };

    const handleKeyPressPronunciation = (e: React.KeyboardEvent) => {
      if (e.key === "Enter") {
        e.preventDefault();
        if (expandedIndex !== null) {
          handleAddPronunciation(expandedIndex);
        }
      }
    };

    const isUpdatingWords = isUpdating("advanced_custom_words");

    return (
      <div className="space-y-3">
        <SettingContainer
          title={t("settings.debug.advancedCustomWords.title")}
          description={t("settings.debug.advancedCustomWords.description")}
          descriptionMode={descriptionMode}
          grouped={grouped}
        >
          <div className="flex items-center gap-2">
            <Input
              type="text"
              className="max-w-48"
              value={newWord}
              onChange={(e) => setNewWord(e.target.value)}
              onKeyDown={handleKeyPressWord}
              placeholder={t("settings.debug.advancedCustomWords.placeholder")}
              variant="compact"
              disabled={isUpdatingWords}
            />
            <Button
              onClick={handleAddWord}
              disabled={!newWord.trim() || newWord.trim().length > 100 || isUpdatingWords}
              variant="primary"
              size="md"
            >
              {t("common.add")}
            </Button>
          </div>
        </SettingContainer>

        {advancedWords.length > 0 && (
          <div className="space-y-2">
            {advancedWords.map((cw: CustomWord, index: number) => (
              <div
                key={cw.word}
                className={`rounded-lg border border-mid-gray/20 ${grouped ? "" : ""}`}
              >
                {/* Word header row */}
                <div
                  className="flex items-center justify-between px-3 py-2 cursor-pointer hover:bg-mid-gray/5 rounded-t-lg"
                  onClick={() => setExpandedIndex(expandedIndex === index ? null : index)}
                >
                  <div className="flex items-center gap-2 flex-1 min-w-0">
                    <svg
                      className={`w-4 h-4 text-text-secondary transition-transform flex-shrink-0 ${
                        expandedIndex === index ? "rotate-90" : ""
                      }`}
                      fill="none"
                      stroke="currentColor"
                      viewBox="0 0 24 24"
                    >
                      <path
                        strokeLinecap="round"
                        strokeLinejoin="round"
                        strokeWidth={2}
                        d="M9 5l7 7-7 7"
                      />
                    </svg>
                    <span className="font-medium text-sm text-text-primary truncate">
                      {cw.word}
                    </span>
                    {cw.pronunciations.length > 0 && (
                      <span className="text-xs text-text-secondary">
                        ({cw.pronunciations.length}{" "}
                        {cw.pronunciations.length === 1
                          ? t("settings.debug.advancedCustomWords.pronunciation")
                          : t("settings.debug.advancedCustomWords.pronunciations")})
                      </span>
                    )}
                  </div>
                  <button
                    onClick={(e) => {
                      e.stopPropagation();
                      handleRemoveWord(index);
                    }}
                    disabled={isUpdatingWords}
                    className="text-text-secondary hover:text-red-500 transition-colors p-1 flex-shrink-0"
                    aria-label={t("settings.advanced.customWords.remove", { word: cw.word })}
                  >
                    <svg className="w-4 h-4" fill="none" stroke="currentColor" viewBox="0 0 24 24">
                      <path
                        strokeLinecap="round"
                        strokeLinejoin="round"
                        strokeWidth={2}
                        d="M6 18L18 6M6 6l12 12"
                      />
                    </svg>
                  </button>
                </div>

                {/* Expanded pronunciation section */}
                {expandedIndex === index && (
                  <div className="px-3 pb-3 pt-1 border-t border-mid-gray/10">
                    <div className="text-xs text-text-secondary mb-2">
                      {t("settings.debug.advancedCustomWords.pronunciationHint")}
                    </div>

                    {/* Existing pronunciations */}
                    {cw.pronunciations.length > 0 && (
                      <div className="flex flex-wrap gap-1 mb-2">
                        {cw.pronunciations.map((pron: string, pronIndex: number) => (
                          <Button
                            key={pronIndex}
                            onClick={() => handleRemovePronunciation(index, pronIndex)}
                            disabled={isUpdatingWords}
                            variant="secondary"
                            size="sm"
                            className="inline-flex items-center gap-1"
                            aria-label={t("settings.debug.advancedCustomWords.removePronunciation", {
                              pronunciation: pron,
                            })}
                          >
                            <span>{pron}</span>
                            <svg className="w-3 h-3" fill="none" stroke="currentColor" viewBox="0 0 24 24">
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

                    {/* Add pronunciation input */}
                    <div className="flex items-center gap-2">
                      <Input
                        type="text"
                        className="max-w-48"
                        value={newPronunciation}
                        onChange={(e) => setNewPronunciation(e.target.value)}
                        onKeyDown={handleKeyPressPronunciation}
                        placeholder={t("settings.debug.advancedCustomWords.pronunciationPlaceholder")}
                        variant="compact"
                        disabled={isUpdatingWords}
                      />
                      <Button
                        onClick={() => handleAddPronunciation(index)}
                        disabled={!newPronunciation.trim() || isUpdatingWords}
                        variant="secondary"
                        size="sm"
                      >
                        {t("settings.debug.advancedCustomWords.addPronunciation")}
                      </Button>
                    </div>
                  </div>
                )}
              </div>
            ))}
          </div>
        )}
      </div>
    );
  },
);