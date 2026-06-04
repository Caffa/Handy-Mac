import React, { useState, useRef, useCallback, useEffect } from "react";
import { useTranslation } from "react-i18next";
import { toast } from "sonner";
import { useSettings } from "../../hooks/useSettings";
import type { CustomWord, PronunciationResult } from "../../bindings";
import { commands } from "../../bindings";
import { listen } from "@tauri-apps/api/event";
import { Input } from "../ui/Input";
import { Button } from "../ui/Button";
import { SettingContainer } from "../ui/SettingContainer";

interface AdvancedCustomWordsProps {
  descriptionMode?: "inline" | "tooltip";
  grouped?: boolean;
}

/** State machine for the pronunciation recording flow */
type RecordingState = "idle" | "recording" | "transcribing" | "multiModel";

interface ModelProgress {
  current: number;
  total: number;
  modelId: string;
  modelName: string;
  completed: boolean;
}

export const AdvancedCustomWords: React.FC<AdvancedCustomWordsProps> =
  React.memo(({ descriptionMode = "tooltip", grouped = false }) => {
    const { t } = useTranslation();
    const { getSetting, updateSetting, isUpdating } = useSettings();
    const advancedWords = getSetting("advanced_custom_words") || [];

    const [newWord, setNewWord] = useState("");
    const [expandedIndex, setExpandedIndex] = useState<number | null>(null);
    const [newPronunciation, setNewPronunciation] = useState("");

    // Pronunciation recording state
    const [recordingState, setRecordingState] =
      useState<RecordingState>("idle");
    const [recordingWordIndex, setRecordingWordIndex] = useState<number | null>(
      null,
    );
    const [modelProgress, setModelProgress] = useState<ModelProgress | null>(
      null,
    );
    const mountedRef = useRef(true);
    const recordingStateRef = useRef<RecordingState>("idle");

    // Keep ref in sync with state (for unmount cleanup)
    useEffect(() => {
      recordingStateRef.current = recordingState;
    }, [recordingState]);

    // Listen for multi-model pronunciation progress events
    useEffect(() => {
      let cancelled = false;
      const setup = async () => {
        const unlisten = await listen(
          "pronunciation-model-progress",
          (event: {
            payload: {
              current: number;
              total: number;
              modelId: string;
              modelName: string;
              completed?: boolean;
              started?: boolean;
            };
          }) => {
            if (cancelled) return;
            const payload = event.payload;
            if (payload.completed) {
              setModelProgress(null);
            } else if (payload.started) {
              setModelProgress({
                current: 0,
                total: 0,
                modelId: "",
                modelName: "Starting...",
                completed: false,
              });
            } else {
              setModelProgress({
                current: payload.current,
                total: payload.total,
                modelId: payload.modelId,
                modelName: payload.modelName,
                completed: false,
              });
            }
          },
        );
        return unlisten;
      };
      let unlistenFn: (() => void) | undefined;
      setup().then((fn) => {
        unlistenFn = fn;
      });
      return () => {
        cancelled = true;
        if (unlistenFn) unlistenFn();
      };
    }, []);

    // Listen for pronunciation processing completion
    useEffect(() => {
      let cancelled = false;
      const setup = async () => {
        const unlisten = await listen(
          "pronunciation-processing-done",
          (event: {
            payload: {
              success: boolean;
              message: string;
              count: number;
              pronunciations?: string[];
              word?: string;
            };
          }) => {
            if (cancelled) return;
            const payload = event.payload;
            if (
              payload.success &&
              payload.count > 0 &&
              payload.pronunciations &&
              payload.word
            ) {
              const currentWords = (getSetting("advanced_custom_words") ||
                []) as CustomWord[];
              const wordIndex = currentWords.findIndex(
                (w: CustomWord) => w.word === payload.word,
              );
              if (wordIndex >= 0) {
                const word = currentWords[wordIndex];
                const existing = new Set(
                  (word.pronunciations ?? []).map((p: string) =>
                    p.toLowerCase(),
                  ),
                );
                const uniqueNew = (payload.pronunciations as string[]).filter(
                  (p: string) => !existing.has(p.toLowerCase()),
                );
                if (uniqueNew.length > 0) {
                  const updated = [...currentWords];
                  updated[wordIndex] = {
                    ...word,
                    pronunciations: [
                      ...(word.pronunciations ?? []),
                      ...uniqueNew,
                    ],
                  };
                  updateSetting("advanced_custom_words", updated);
                  toast.success(
                    t("settings.debug.advancedCustomWords.multiModelSuccess", {
                      count: uniqueNew.length,
                      word: payload.word,
                    }),
                  );
                } else {
                  toast.info(
                    t("settings.debug.advancedCustomWords.allModelsDuplicate", {
                      word: payload.word,
                    }),
                  );
                }
              }
            } else if (payload.success) {
              toast.info(payload.message);
            } else {
              toast.error(
                t("settings.debug.advancedCustomWords.recordingError", {
                  error: payload.message,
                }),
              );
            }
            setModelProgress(null);
          },
        );
        return unlisten;
      };
      let unlistenFn: (() => void) | undefined;
      setup().then((fn) => {
        unlistenFn = fn;
      });
      return () => {
        cancelled = true;
        if (unlistenFn) unlistenFn();
      };
    }, []);

    // Cleanup on unmount — stop any active pronunciation recording
    useEffect(() => {
      mountedRef.current = true;
      return () => {
        mountedRef.current = false;
        // If we're recording, cancel it on the backend to avoid orphaned state
        if (recordingStateRef.current === "recording") {
          commands.cancelPronunciationRecording().catch(() => {});
        }
      };
    }, []);

    const handleAddWord = () => {
      const trimmedWord = newWord.trim();
      // Remove punctuation and special characters from the word
      const sanitizedWord = trimmedWord.replace(/[<>"'&.,!?;:'"()\[\]{}@#$%^&*+=|\\/_~`]/g, "");
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
      const updated = advancedWords.filter(
        (_: CustomWord, i: number) => i !== index,
      );
      updateSetting("advanced_custom_words", updated);
      if (expandedIndex === index) {
        setExpandedIndex(null);
      } else if (expandedIndex !== null && expandedIndex > index) {
        setExpandedIndex(expandedIndex - 1);
      }
    };

    const handleAddPronunciation = (
      wordIndex: number,
      pronunciation?: string,
    ) => {
      const trimmed = (pronunciation ?? newPronunciation).trim();
      if (!trimmed) return;

      // Remove punctuation from the pronunciation for cleaner matching
      const sanitizedPronunciation = trimmed.replace(/[<>"'&.,!?;:'"()\[\]{}@#$%^&*+=|\\/_~`]/g, "");
      if (!sanitizedPronunciation) return;

      const updated = [...advancedWords];
      const word = updated[wordIndex];
      if (!word) return;
      const pronunciations = word.pronunciations ?? [];
      if (pronunciations.includes(sanitizedPronunciation)) {
        toast.error(
          t("settings.debug.advancedCustomWords.duplicatePronunciation", {
            pronunciation: sanitizedPronunciation,
          }),
        );
        return;
      }
      updated[wordIndex] = {
        ...word,
        pronunciations: [...pronunciations, sanitizedPronunciation],
      };
      updateSetting("advanced_custom_words", updated);
      setNewPronunciation("");
    };

    const handleRemovePronunciation = (
      wordIndex: number,
      pronIndex: number,
    ) => {
      const updated = [...advancedWords];
      const word = updated[wordIndex];
      if (!word) return;
      const pronunciations = word.pronunciations ?? [];
      updated[wordIndex] = {
        ...word,
        pronunciations: pronunciations.filter(
          (_: string, i: number) => i !== pronIndex,
        ),
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

    // ── Pronunciation recording flow ──────────────────────────────────

    const stopAndTranscribe = useCallback(
      async (wordIndex: number) => {
        setRecordingState("transcribing");

        try {
          const currentWords = (getSetting("advanced_custom_words") ||
            []) as CustomWord[];
          const word = currentWords[wordIndex];
          if (!word) {
            toast.error(
              t("settings.debug.advancedCustomWords.recordingError", {
                error: "Word no longer exists",
              }),
            );
            return;
          }

          const result = await commands.stopAndSchedulePronunciation(word.word);
          if (result.status === "ok") {
            toast.info(result.data);
            // Reset state immediately - processing will happen in background
            if (mountedRef.current) {
              setRecordingState("idle");
              setRecordingWordIndex(null);
            }
          } else {
            toast.error(
              t("settings.debug.advancedCustomWords.recordingError", {
                error:
                  result.status === "error" ? result.error : "Unknown error",
              }),
            );
            if (mountedRef.current) {
              setRecordingState("idle");
              setRecordingWordIndex(null);
            }
          }
        } catch (e) {
          toast.error(
            t("settings.debug.advancedCustomWords.recordingError", {
              error: e instanceof Error ? e.message : String(e),
            }),
          );
          if (mountedRef.current) {
            setRecordingState("idle");
            setRecordingWordIndex(null);
          }
        }
      },
      // eslint-disable-next-line react-hooks/exhaustive-deps
      [t, getSetting, updateSetting],
    );

    const handleStartRecording = useCallback(
      async (wordIndex: number) => {
        try {
          const result = await commands.startPronunciationRecording();
          if (result.status === "ok") {
            setRecordingState("recording");
            setRecordingWordIndex(wordIndex);
          } else {
            toast.error(
              t("settings.debug.advancedCustomWords.recordingStartError", {
                error:
                  result.status === "error" ? result.error : "Unknown error",
              }),
            );
          }
        } catch (e) {
          toast.error(
            t("settings.debug.advancedCustomWords.recordingStartError", {
              error: e instanceof Error ? e.message : String(e),
            }),
          );
        }
      },
      [t],
    );

    const handleStopRecording = useCallback(() => {
      if (recordingWordIndex !== null) {
        stopAndTranscribe(recordingWordIndex);
      }
    }, [recordingWordIndex, stopAndTranscribe]);

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
              disabled={
                !newWord.trim() ||
                newWord.trim().length > 100 ||
                isUpdatingWords
              }
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
                  onClick={() =>
                    setExpandedIndex(expandedIndex === index ? null : index)
                  }
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
                    {(cw.pronunciations ?? []).length > 0 && (
                      <span className="text-xs text-text-secondary">
                        ({(cw.pronunciations ?? []).length}{" "}
                        {(cw.pronunciations ?? []).length === 1
                          ? t(
                              "settings.debug.advancedCustomWords.pronunciation",
                            )
                          : t(
                              "settings.debug.advancedCustomWords.pronunciations",
                            )}
                        )
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
                    aria-label={t("settings.advanced.customWords.remove", {
                      word: cw.word,
                    })}
                  >
                    <svg
                      className="w-4 h-4"
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
                  </button>
                </div>

                {/* Expanded pronunciation section */}
                {expandedIndex === index && (
                  <div className="px-3 pb-3 pt-1 border-t border-mid-gray/10">
                    <div className="text-xs text-text-secondary mb-2">
                      {t(
                        "settings.debug.advancedCustomWords.pronunciationHint",
                      )}
                    </div>

                    {/* Existing pronunciations */}
                    {(cw.pronunciations ?? []).length > 0 && (
                      <div className="flex flex-wrap gap-1 mb-2">
                        {(cw.pronunciations ?? []).map(
                          (pron: string, pronIndex: number) => (
                            <Button
                              key={pronIndex}
                              onClick={() =>
                                handleRemovePronunciation(index, pronIndex)
                              }
                              disabled={isUpdatingWords}
                              variant="secondary"
                              size="sm"
                              className="inline-flex items-center gap-1"
                              aria-label={t(
                                "settings.debug.advancedCustomWords.removePronunciation",
                                {
                                  pronunciation: pron,
                                },
                              )}
                            >
                              <span>{pron}</span>
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
                          ),
                        )}
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
                        placeholder={t(
                          "settings.debug.advancedCustomWords.pronunciationPlaceholder",
                        )}
                        variant="compact"
                        disabled={isUpdatingWords}
                      />
                      <Button
                        onClick={() => handleAddPronunciation(index)}
                        disabled={!newPronunciation.trim() || isUpdatingWords}
                        variant="secondary"
                        size="sm"
                      >
                        {t(
                          "settings.debug.advancedCustomWords.addPronunciation",
                        )}
                      </Button>
                    </div>

                    {/* Record pronunciation */}
                    <div className="mt-2 flex items-center gap-2">
                      {recordingState === "idle" ||
                      recordingWordIndex !== index ? (
                        <Button
                          onClick={() => handleStartRecording(index)}
                          disabled={
                            recordingState !== "idle" || isUpdatingWords
                          }
                          variant="secondary"
                          size="sm"
                          className="inline-flex items-center gap-1.5"
                          title={t(
                            "settings.debug.advancedCustomWords.recordTooltip",
                          )}
                        >
                          {/* Microphone icon */}
                          <svg
                            className="w-4 h-4"
                            fill="none"
                            stroke="currentColor"
                            viewBox="0 0 24 24"
                          >
                            <path
                              strokeLinecap="round"
                              strokeLinejoin="round"
                              strokeWidth={2}
                              d="M19 11a7 7 0 01-7 7m0 0a7 7 0 01-7-7m7 7v4m0 0H8m4 0h4m-4-8a3 3 0 01-3-3V5a3 3 0 116 0v6a3 3 0 01-3 3z"
                            />
                          </svg>
                          <span>
                            {t(
                              "settings.debug.advancedCustomWords.recordPronunciation",
                            )}
                          </span>
                        </Button>
                      ) : recordingState === "recording" ? (
                        <Button
                          onClick={handleStopRecording}
                          variant="primary"
                          size="sm"
                          className="inline-flex items-center gap-1.5"
                        >
                          <span className="relative flex h-3 w-3">
                            <span className="animate-ping absolute inline-flex h-full w-full rounded-full bg-red-400 opacity-75" />
                            <span className="relative inline-flex rounded-full h-3 w-3 bg-red-500" />
                          </span>
                          <span>
                            {t(
                              "settings.debug.advancedCustomWords.stopRecording",
                            )}
                          </span>
                        </Button>
                      ) : modelProgress ? (
                        <div className="flex items-center gap-2 text-sm text-text-secondary">
                          <svg
                            className="w-4 h-4 animate-spin"
                            fill="none"
                            viewBox="0 0 24 24"
                          >
                            <circle
                              className="opacity-25"
                              cx="12"
                              cy="12"
                              r="10"
                              stroke="currentColor"
                              strokeWidth="4"
                            />
                            <path
                              className="opacity-75"
                              fill="currentColor"
                              d="M4 12a8 8 0 018-8V0C5.373 0 0 5.373 0 12h4zm2 5.291A7.962 7.962 0 014 12H0c0 3.042 1.135 5.824 3 7.938l3-2.647z"
                            />
                          </svg>
                          <span>
                            {t(
                              "settings.debug.advancedCustomWords.multiModelProgress",
                              {
                                current: modelProgress.current,
                                total: modelProgress.total,
                                modelName: modelProgress.modelName,
                              },
                            )}
                          </span>
                        </div>
                      ) : (
                        <Button
                          disabled
                          variant="secondary"
                          size="sm"
                          className="inline-flex items-center gap-1.5"
                        >
                          <svg
                            className="w-4 h-4 animate-spin"
                            fill="none"
                            viewBox="0 0 24 24"
                          >
                            <circle
                              className="opacity-25"
                              cx="12"
                              cy="12"
                              r="10"
                              stroke="currentColor"
                              strokeWidth="4"
                            />
                            <path
                              className="opacity-75"
                              fill="currentColor"
                              d="M4 12a8 8 0 018-8V0C5.373 0 0 5.373 0 12h4zm2 5.291A7.962 7.962 0 014 12H0c0 3.042 1.135 5.824 3 7.938l3-2.647z"
                            />
                          </svg>
                          <span>
                            {t(
                              "settings.debug.advancedCustomWords.transcribing",
                            )}
                          </span>
                        </Button>
                      )}
                    </div>
                  </div>
                )}
              </div>
            ))}
          </div>
        )}
      </div>
    );
  });
