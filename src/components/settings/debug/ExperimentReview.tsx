import React, { useState, useEffect, useRef } from "react";
import { useTranslation } from "react-i18next";
import {
  commands,
  type HistoryEntry,
  type ExperimentGroup,
  type TranscriptionVariant,
  type ModelInfo,
  type GeneratedVariant,
} from "@/bindings";
import { SettingsGroup } from "../../ui/SettingsGroup";
import { Button } from "../../ui/Button";
import { Check, X, GripVertical, Plus, Download, Loader2 } from "lucide-react";
import { save } from "@tauri-apps/plugin-dialog";
import { writeFile } from "@tauri-apps/plugin-fs";

interface ExperimentCardProps {
  entry: HistoryEntry;
  experimentGroup: ExperimentGroup | null;
  variants: TranscriptionVariant[];
  availableModels: ModelInfo[];
  onCreateExperiment: () => void;
  onUpdateGroundTruth: (text: string) => void;
  onGenerateVariants: (models: string[]) => Promise<void>;
  onUpdateVariant: (
    id: number,
    ranking: number | null,
    is_acceptable: boolean | null,
    notes: string | null,
    match_score: number | null,
  ) => void;
  onUpdateMetadata: (speech_speed: string, recording_quality: string) => void;
}

const ExperimentCard: React.FC<ExperimentCardProps> = ({
  entry,
  experimentGroup,
  variants,
  availableModels,
  onCreateExperiment,
  onUpdateGroundTruth,
  onGenerateVariants,
  onUpdateVariant,
  onUpdateMetadata,
}) => {
  const { t } = useTranslation();
  const [groundTruth, setGroundTruth] = useState(
    experimentGroup?.ground_truth || entry.transcription_text,
  );
  const [editingGroundTruth, setEditingGroundTruth] = useState(false);
  const [speechSpeed, setSpeechSpeed] = useState(
    experimentGroup?.speech_speed || "normal",
  );
  const [recordingQuality, setRecordingQuality] = useState(
    experimentGroup?.recording_quality || "good",
  );
  const [draggedIndex, setDraggedIndex] = useState<number | null>(null);
  const [isGenerating, setIsGenerating] = useState(false);
  const [selectedModels, setSelectedModels] = useState<string[]>([]);

  const handleDragStart = (index: number) => {
    setDraggedIndex(index);
  };

  const handleDragOver = async (e: React.DragEvent, index: number) => {
    e.preventDefault();
    if (draggedIndex === null || draggedIndex === index) return;

    const newVariants = [...variants];
    const draggedVariant = newVariants[draggedIndex];
    newVariants.splice(draggedIndex, 1);
    newVariants.splice(index, 0, draggedVariant);

    // Update rankings sequentially to ensure consistency
    for (let i = 0; i < newVariants.length; i++) {
      if (newVariants[i].ranking !== i + 1) {
        await onUpdateVariant(newVariants[i].id, i + 1, null, null, null);
      }
    }

    setDraggedIndex(null);
  };

  const calculateMatchScore = (text: string, groundTruth: string): number => {
    const a = text.toLowerCase().trim();
    const b = groundTruth.toLowerCase().trim();
    if (a === b) return 100;

    // Simple word-level similarity
    const wordsA = a.split(/\s+/).filter((w) => w.length > 0);
    const wordsB = b.split(/\s+/).filter((w) => w.length > 0);

    // Handle empty strings
    if (wordsA.length === 0 && wordsB.length === 0) return 100;
    if (wordsA.length === 0 || wordsB.length === 0) return 0;

    const common = wordsA.filter((w) => wordsB.includes(w)).length;
    const total = Math.max(wordsA.length, wordsB.length);
    return Math.round((common / total) * 100);
  };

  if (!experimentGroup) {
    return (
      <div className="p-4 border border-border rounded-lg bg-surface">
        <div className="flex justify-between items-start mb-2">
          <div className="flex-1">
            <div className="flex items-center gap-2 mb-1">
              <span className="text-xs text-text/50">#{entry.id}</span>
              <span className="text-xs text-text/40">
                {new Date(entry.timestamp * 1000).toLocaleDateString()}
              </span>
            </div>
            <p className="text-sm text-text">{entry.transcription_text}</p>
          </div>
          <Button variant="secondary" size="sm" onClick={onCreateExperiment}>
            <Plus className="w-4 h-4 mr-1" />
            Create Experiment
          </Button>
        </div>
      </div>
    );
  }

  return (
    <div className="p-4 border border-border rounded-lg bg-surface space-y-4">
      {/* Header */}
      <div className="flex justify-between items-start">
        <div className="flex-1">
          <div className="flex items-center gap-2 mb-1">
            <span className="text-xs text-text/50">#{entry.id}</span>
            <span className="text-xs text-text/40">Experiment</span>
          </div>

          {/* Ground Truth */}
          {editingGroundTruth ? (
            <div className="space-y-2">
              <textarea
                className="w-full p-2 border border-border rounded bg-surface text-text text-sm"
                value={groundTruth}
                onChange={(e) => setGroundTruth(e.target.value)}
                rows={2}
                placeholder="Corrected text..."
              />
              <div className="flex gap-2">
                <Button
                  size="sm"
                  onClick={() => {
                    onUpdateGroundTruth(groundTruth);
                    setEditingGroundTruth(false);
                  }}
                >
                  <Check className="w-4 h-4 mr-1" />
                  Save
                </Button>
                <Button
                  variant="secondary"
                  size="sm"
                  onClick={() => {
                    setGroundTruth(
                      experimentGroup?.ground_truth || entry.transcription_text,
                    );
                    setEditingGroundTruth(false);
                  }}
                >
                  Cancel
                </Button>
              </div>
            </div>
          ) : (
            <div
              className="cursor-pointer"
              onClick={() => setEditingGroundTruth(true)}
            >
              <p className="text-sm font-medium text-primary">
                Ground Truth: {groundTruth}
              </p>
              <p className="text-xs text-text/40">Click to edit</p>
            </div>
          )}
        </div>
      </div>

      {/* Metadata */}
      <div className="flex gap-4">
        <div className="space-y-1">
          <label className="text-xs text-text/60">Speech Speed</label>
          <select
            className="p-1 border border-border rounded bg-surface text-text text-sm"
            value={speechSpeed}
            onChange={(e) => {
              setSpeechSpeed(e.target.value);
              onUpdateMetadata(e.target.value, recordingQuality);
            }}
          >
            <option value="very-slow">Very Slow</option>
            <option value="slow">Slow</option>
            <option value="normal">Normal</option>
            <option value="fast">Fast</option>
            <option value="very-fast">Very Fast</option>
          </select>
        </div>

        <div className="space-y-1">
          <label className="text-xs text-text/60">Recording Quality</label>
          <select
            className="p-1 border border-border rounded bg-surface text-text text-sm"
            value={recordingQuality}
            onChange={(e) => {
              setRecordingQuality(e.target.value);
              onUpdateMetadata(speechSpeed, e.target.value);
            }}
          >
            <option value="poor">Poor</option>
            <option value="fair">Fair</option>
            <option value="good">Good</option>
            <option value="excellent">Excellent</option>
          </select>
        </div>
      </div>

      {/* Variants */}
      <div className="space-y-2">
        <div className="flex justify-between items-center">
          <h4 className="text-sm font-medium">Transcription Variants</h4>
          {isGenerating ? (
            <div className="flex items-center gap-2 text-sm text-text/60">
              <Loader2 className="w-4 h-4 animate-spin" />
              Generating...
            </div>
          ) : (
            <Button
              variant="secondary"
              size="sm"
              onClick={async () => {
                if (!experimentGroup) return;

                // Default to using all downloaded models
                const downloadedModels = availableModels.filter(
                  (m) => m.is_downloaded,
                );
                const modelIds =
                  downloadedModels.length > 0
                    ? downloadedModels.slice(0, 5).map((m) => m.id)
                    : ["turbo", "medium", "small"]; // Fallback defaults

                setIsGenerating(true);
                try {
                  await onGenerateVariants(modelIds);
                } finally {
                  setIsGenerating(false);
                }
              }}
              disabled={!experimentGroup?.ground_truth?.trim()}
            >
              <Plus className="w-4 h-4 mr-1" />
              Generate Variants
            </Button>
          )}
        </div>

        {!experimentGroup?.ground_truth && (
          <p className="text-xs text-amber-500/80">
            ⚠️ Set ground truth before generating variants
          </p>
        )}

        {variants.length === 0 ? (
          <p className="text-sm text-text/60 text-center py-4">
            No variants yet. Edit ground truth above, then click "Generate
            Variants" to test different models.
          </p>
        ) : (
          <div className="space-y-2">
            {variants
              .sort((a, b) => (a.ranking || 999) - (b.ranking || 999))
              .map((variant, index) => {
                const score = groundTruth
                  ? calculateMatchScore(variant.transcription_text, groundTruth)
                  : null;

                return (
                  <div
                    key={variant.id}
                    className="p-3 border border-border rounded bg-surface-secondary space-y-2"
                    draggable
                    onDragStart={() => handleDragStart(index)}
                    onDragOver={(e) => handleDragOver(e, index)}
                  >
                    <div className="flex items-start gap-2">
                      <div className="cursor-move text-text/40 hover:text-text">
                        <GripVertical className="w-4 h-4" />
                      </div>

                      <div className="flex-1 space-y-1">
                        <div className="flex justify-between items-center">
                          <span className="text-xs font-mono text-text/60">
                            {variant.model_id}
                          </span>
                          <div className="flex items-center gap-2">
                            {score !== null && (
                              <span className="text-xs px-2 py-0.5 rounded bg-primary/10 text-primary">
                                {score}% match
                              </span>
                            )}
                            <button
                              className={`p-1 rounded ${
                                variant.is_acceptable
                                  ? "bg-green-500/20 text-green-500"
                                  : "bg-red-500/20 text-red-500"
                              }`}
                              onClick={() =>
                                onUpdateVariant(
                                  variant.id,
                                  null,
                                  !variant.is_acceptable,
                                  null,
                                  null,
                                )
                              }
                            >
                              {variant.is_acceptable ? (
                                <Check className="w-3 h-3" />
                              ) : (
                                <X className="w-3 h-3" />
                              )}
                            </button>
                          </div>
                        </div>

                        <p className="text-sm text-text">
                          {variant.transcription_text}
                        </p>

                        {variant.notes && (
                          <p className="text-xs text-text/60">
                            Notes: {variant.notes}
                          </p>
                        )}
                      </div>
                    </div>

                    <div className="flex items-center gap-2">
                      <span className="text-xs text-text/40">Rank:</span>
                      {[1, 2, 3, 4, 5].map((rank) => (
                        <button
                          key={rank}
                          className={`w-6 h-6 rounded text-xs ${
                            variant.ranking === rank
                              ? "bg-primary text-white"
                              : "bg-surface text-text/60 hover:text-text"
                          }`}
                          onClick={() =>
                            onUpdateVariant(variant.id, rank, null, null, null)
                          }
                        >
                          {rank}
                        </button>
                      ))}
                    </div>
                  </div>
                );
              })}
          </div>
        )}
      </div>
    </div>
  );
};

export const ExperimentReview: React.FC = () => {
  const { t } = useTranslation();
  const [entries, setEntries] = useState<HistoryEntry[]>([]);
  const [experiments, setExperiments] = useState<Map<number, ExperimentGroup>>(
    new Map(),
  );
  const [variants, setVariants] = useState<Map<number, TranscriptionVariant[]>>(
    new Map(),
  );
  const [availableModels, setAvailableModels] = useState<ModelInfo[]>([]);

  // Load available models
  useEffect(() => {
    const loadModels = async () => {
      try {
        const result = await commands.getAvailableModels();
        if (result.status === "ok") {
          setAvailableModels(result.data.filter((m) => m.is_downloaded));
        }
      } catch (e) {
        console.error("Failed to load models:", e);
      }
    };
    loadModels();
  }, []);

  // Load saved recordings
  useEffect(() => {
    const abortController = new AbortController();

    const loadEntries = async () => {
      try {
        const result = await commands.getHistoryEntries(null, 50);
        if (result.status === "ok") {
          const saved = result.data.entries.filter((e) => e.saved);
          setEntries(saved);

          // Load experiment groups for each saved recording
          for (const entry of saved) {
            if (abortController.signal.aborted) break;

            const expResult = await commands.getExperimentGroup(entry.id);
            if (expResult.status === "ok" && expResult.data) {
              setExperiments((prev) => {
                const next = new Map(prev);
                next.set(entry.id, expResult.data!);
                return next;
              });

              // Load variants
              const varResult = await commands.getVariantsForExperiment(
                expResult.data!.id,
              );
              if (varResult.status === "ok") {
                setVariants((prev) => {
                  const next = new Map(prev);
                  next.set(entry.id, varResult.data);
                  return next;
                });
              }
            }
          }
        }
      } catch (e) {
        if (!abortController.signal.aborted) {
          console.error("Failed to load entries:", e);
        }
      }
    };
    loadEntries();

    return () => abortController.abort();
  }, []);

  const handleCreateExperiment = async (entry: HistoryEntry) => {
    try {
      const result = await commands.createExperimentGroup(entry.id);
      if (result.status === "ok") {
        setExperiments((prev) => {
          const next = new Map(prev);
          next.set(entry.id, result.data);
          return next;
        });
      }
    } catch (e) {
      console.error("Failed to create experiment:", e);
    }
  };

  const handleUpdateGroundTruth = async (
    entryId: number,
    groundTruth: string,
  ) => {
    const exp = experiments.get(entryId);
    if (!exp) return;

    try {
      const result = await commands.updateExperimentGroup(
        exp.id,
        groundTruth,
        null,
        null,
        null,
        null,
      );
      if (result.status === "ok") {
        setExperiments((prev) => {
          const next = new Map(prev);
          next.set(entryId, result.data);
          return next;
        });

        // Update match scores for all variants
        const vars = variants.get(entryId) || [];
        for (const v of vars) {
          // Calculate score and update
        }
      }
    } catch (e) {
      console.error("Failed to update ground truth:", e);
    }
  };

  const handleGenerateVariants = async (entryId: number, models: string[]) => {
    const exp = experiments.get(entryId);
    if (!exp) return;

    try {
      const result = await commands.generateVariants(exp.id, models);
      if (result.status === "ok") {
        // Add each variant to the database
        for (const generated of result.data) {
          await commands.addTranscriptionVariant(
            exp.id,
            generated.model_id,
            generated.parameters,
            generated.transcription_text,
          );
        }

        // Reload variants
        const varResult = await commands.getVariantsForExperiment(exp.id);
        if (varResult.status === "ok") {
          setVariants((prev) => {
            const next = new Map(prev);
            next.set(entryId, varResult.data);
            return next;
          });
        }
      }
    } catch (e) {
      console.error("Failed to generate variants:", e);
    }
  };

  const handleExportDataset = async () => {
    try {
      const filePath = await save({
        defaultPath: `experiment-dataset-${Date.now()}.json`,
        filters: [{ name: "JSON", extensions: ["json"] }],
      });

      if (!filePath) return;

      const dataset = [];
      for (const entry of entries) {
        const exp = experiments.get(entry.id);
        if (!exp || !exp.ground_truth) continue;

        const vars = variants.get(entry.id) || [];
        dataset.push({
          recording_id: entry.id,
          ground_truth: exp.ground_truth,
          speech_speed: exp.speech_speed,
          recording_quality: exp.recording_quality,
          variants: vars.map((v) => ({
            model_id: v.model_id,
            parameters: v.parameters,
            transcription: v.transcription_text,
            match_score: v.match_score,
            ranking: v.ranking,
            is_acceptable: v.is_acceptable,
          })),
        });
      }

      await writeFile(
        filePath,
        new TextEncoder().encode(JSON.stringify(dataset, null, 2)),
      );
    } catch (e) {
      console.error("Failed to export dataset:", e);
    }
  };

  return (
    <SettingsGroup title="Experiment Review">
      <div className="space-y-4">
        <div className="flex justify-between items-center">
          <p className="text-sm text-text/70">
            Review saved recordings, compare transcription variants, and mark
            ground truth for accuracy testing.
          </p>
          <Button variant="secondary" size="sm" onClick={handleExportDataset}>
            <Download className="w-4 h-4 mr-1" />
            Export Dataset
          </Button>
        </div>

        {entries.length === 0 ? (
          <p className="text-sm text-text/60 text-center py-4">
            No saved recordings yet. Star some recordings to create experiments.
          </p>
        ) : (
          <div className="space-y-4">
            {entries.map((entry) => (
              <ExperimentCard
                key={entry.id}
                entry={entry}
                experimentGroup={experiments.get(entry.id) || null}
                variants={variants.get(entry.id) || []}
                availableModels={availableModels}
                onCreateExperiment={() => handleCreateExperiment(entry)}
                onUpdateGroundTruth={(text) =>
                  handleUpdateGroundTruth(entry.id, text)
                }
                onGenerateVariants={(models) =>
                  handleGenerateVariants(entry.id, models)
                }
                onUpdateVariant={async (
                  id,
                  ranking,
                  is_acceptable,
                  notes,
                  match_score,
                ) => {
                  try {
                    const result = await commands.updateTranscriptionVariant(
                      id,
                      ranking,
                      is_acceptable,
                      notes,
                      match_score,
                    );
                    if (result.status === "ok") {
                      setVariants((prev) => {
                        const next = new Map(prev);
                        // Create new array to avoid mutating original
                        const entryVars = [...(next.get(entry.id) || [])];
                        const idx = entryVars.findIndex((v) => v.id === id);
                        if (idx >= 0) {
                          entryVars[idx] = result.data;
                          next.set(entry.id, entryVars);
                        }
                        return next;
                      });
                    }
                  } catch (e) {
                    console.error("Failed to update variant:", e);
                  }
                }}
                onUpdateMetadata={async (speech_speed, recording_quality) => {
                  const exp = experiments.get(entry.id);
                  if (!exp) return;
                  try {
                    const result = await commands.updateExperimentGroup(
                      exp.id,
                      null,
                      speech_speed,
                      recording_quality,
                      null,
                      null,
                    );
                    if (result.status === "ok") {
                      setExperiments((prev) => {
                        const next = new Map(prev);
                        next.set(entry.id, result.data);
                        return next;
                      });
                    }
                  } catch (e) {
                    console.error("Failed to update metadata:", e);
                  }
                }}
              />
            ))}
          </div>
        )}
      </div>
    </SettingsGroup>
  );
};
