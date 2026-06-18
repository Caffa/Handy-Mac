import React, {
  useCallback,
  useEffect,
  useMemo,
  useRef,
  useState,
} from "react";
import { convertFileSrc } from "@tauri-apps/api/core";
import { readFile } from "@tauri-apps/plugin-fs";
import {
  Check,
  Copy,
  FolderOpen,
  RotateCcw,
  Star,
  Trash2,
  Search,
  Loader2,
  Play,
  X,
} from "lucide-react";
import { useTranslation } from "react-i18next";
import { toast } from "sonner";
import {
  commands,
  events,
  type HistoryEntry,
  type HistoryUpdatePayload,
  type ExperimentGroup,
  type TranscriptionVariant,
  type ModelInfo,
} from "@/bindings";
import { useOsType } from "@/hooks/useOsType";
import { formatDateTime } from "@/utils/dateFormat";
import { AudioPlayer } from "../../ui/AudioPlayer";
import Badge from "../../ui/Badge";
import { Button } from "../../ui/Button";
import { Input } from "../../ui/Input";

const IconButton: React.FC<{
  onClick: () => void;
  title: string;
  disabled?: boolean;
  active?: boolean;
  children: React.ReactNode;
}> = ({ onClick, title, disabled, active, children }) => (
  <button
    onClick={onClick}
    disabled={disabled}
    className={`p-1.5 rounded-md flex items-center justify-center transition-colors cursor-pointer disabled:cursor-not-allowed disabled:text-text/20 ${
      active
        ? "text-logo-primary hover:text-logo-primary/80"
        : "text-text/50 hover:text-logo-primary"
    }`}
    title={title}
  >
    {children}
  </button>
);

const PAGE_SIZE = 30;

interface OpenRecordingsButtonProps {
  onClick: () => void;
  label: string;
}

const OpenRecordingsButton: React.FC<OpenRecordingsButtonProps> = ({
  onClick,
  label,
}) => (
  <Button
    onClick={onClick}
    variant="secondary"
    size="sm"
    className="flex items-center gap-2"
    title={label}
  >
    <FolderOpen className="w-4 h-4" />
    <span>{label}</span>
  </Button>
);

export const HistorySettings: React.FC = () => {
  const { t } = useTranslation();
  const osType = useOsType();
  const [entries, setEntries] = useState<HistoryEntry[]>([]);
  const [loading, setLoading] = useState(true);
  const [hasMore, setHasMore] = useState(true);
  const [searchQuery, setSearchQuery] = useState("");
  const sentinelRef = useRef<HTMLDivElement>(null);
  const entriesRef = useRef<HistoryEntry[]>([]);
  const loadingRef = useRef(false);
  
  // Experiment state
  const [experiments, setExperiments] = useState<Map<number, ExperimentGroup>>(new Map());
  const [variants, setVariants] = useState<Map<number, TranscriptionVariant[]>>(new Map());
  const [availableModels, setAvailableModels] = useState<ModelInfo[]>([]);
  const [generatingVariants, setGeneratingVariants] = useState<Set<number>>(new Set());

  // Keep ref in sync for use in IntersectionObserver callback
  useEffect(() => {
    entriesRef.current = entries;
  }, [entries]);

  // Load available models
  useEffect(() => {
    const loadModels = async () => {
      try {
        const result = await commands.getAvailableModels();
        if (result.status === "ok") {
          setAvailableModels(result.data.filter(m => m.is_downloaded));
        }
      } catch (e) {
        console.error("Failed to load models:", e);
      }
    };
    loadModels();
  }, []);

  // Load experiments for saved entries
  useEffect(() => {
    const loadExperiments = async () => {
      const saved = entries.filter(e => e.saved);
      for (const entry of saved) {
        try {
          const expResult = await commands.getExperimentGroup(entry.id);
          if (expResult.status === "ok" && expResult.data) {
            setExperiments(prev => {
              const next = new Map(prev);
              next.set(entry.id, expResult.data!);
              return next;
            });

            // Load variants
            const varResult = await commands.getVariantsForExperiment(expResult.data!.id);
            if (varResult.status === "ok") {
              setVariants(prev => {
                const next = new Map(prev);
                next.set(entry.id, varResult.data);
                return next;
              });
            }
          }
        } catch (e) {
          console.error("Failed to load experiment:", e);
        }
      }
    };
    
    if (entries.length > 0) {
      loadExperiments();
    }
  }, [entries]);

  const loadPage = useCallback(async (cursor?: number) => {
    const isFirstPage = cursor === undefined;
    if (!isFirstPage && loadingRef.current) return;
    loadingRef.current = true;

    if (isFirstPage) setLoading(true);

    try {
      const result = await commands.getHistoryEntries(
        cursor ?? null,
        PAGE_SIZE,
      );
      if (result.status === "ok") {
        const { entries: newEntries, has_more } = result.data;
        setEntries((prev) =>
          isFirstPage ? newEntries : [...prev, ...newEntries],
        );
        setHasMore(has_more);
      }
    } catch (error) {
      console.error("Failed to load history entries:", error);
    } finally {
      setLoading(false);
      loadingRef.current = false;
    }
  }, []);

  // Filter entries by search query
  const filteredEntries = useMemo(() => {
    if (!searchQuery.trim()) return entries;
    const query = searchQuery.toLowerCase();
    return entries.filter((entry) =>
      entry.transcription_text.toLowerCase().includes(query),
    );
  }, [entries, searchQuery]);

  // Initial load
  useEffect(() => {
    loadPage();
  }, [loadPage]);

  // Infinite scroll via IntersectionObserver
  useEffect(() => {
    if (loading) return;

    const sentinel = sentinelRef.current;
    if (!sentinel || !hasMore) return;

    const observer = new IntersectionObserver(
      (observerEntries) => {
        const first = observerEntries[0];
        if (first.isIntersecting) {
          const lastEntry = entriesRef.current[entriesRef.current.length - 1];
          if (lastEntry) {
            loadPage(lastEntry.id);
          }
        }
      },
      { threshold: 0 },
    );

    observer.observe(sentinel);
    return () => observer.disconnect();
  }, [loading, hasMore, loadPage]);

  // Listen for new entries added from the transcription pipeline
  useEffect(() => {
    const unlisten = events.historyUpdatePayload.listen((event) => {
      const payload: HistoryUpdatePayload = event.payload;
      if (payload.action === "added") {
        setEntries((prev) => [payload.entry, ...prev]);
      } else if (payload.action === "updated") {
        setEntries((prev) =>
          prev.map((e) => (e.id === payload.entry.id ? payload.entry : e)),
        );
      }
      // "deleted" and "toggled" are handled by optimistic updates only,
      // so we intentionally ignore them here to avoid double-mutation.
    });

    return () => {
      unlisten.then((fn) => fn());
    };
  }, []);

  const toggleSaved = async (id: number) => {
    // Optimistic update
    setEntries((prev) =>
      prev.map((e) => (e.id === id ? { ...e, saved: !e.saved } : e)),
    );
    try {
      const result = await commands.toggleHistoryEntrySaved(id);
      if (result.status !== "ok") {
        // Revert on failure
        setEntries((prev) =>
          prev.map((e) => (e.id === id ? { ...e, saved: !e.saved } : e)),
        );
      }
    } catch (error) {
      console.error("Failed to toggle saved status:", error);
      // Revert on failure
      setEntries((prev) =>
        prev.map((e) => (e.id === id ? { ...e, saved: !e.saved } : e)),
      );
    }
  };

  const copyToClipboard = async (text: string) => {
    try {
      await navigator.clipboard.writeText(text);
    } catch (error) {
      console.error("Failed to copy to clipboard:", error);
    }
  };

  const getAudioUrl = useCallback(
    async (fileName: string) => {
      try {
        const result = await commands.getAudioFilePath(fileName);
        if (result.status === "ok") {
          if (osType === "linux") {
            const fileData = await readFile(result.data);
            const blob = new Blob([fileData], { type: "audio/wav" });
            return URL.createObjectURL(blob);
          }
          return convertFileSrc(result.data, "asset");
        }
        return null;
      } catch (error) {
        console.error("Failed to get audio file path:", error);
        return null;
      }
    },
    [osType],
  );

  const deleteAudioEntry = async (id: number) => {
    // Optimistically remove
    setEntries((prev) => prev.filter((e) => e.id !== id));
    try {
      const result = await commands.deleteHistoryEntry(id);
      if (result.status !== "ok") {
        // Reload on failure
        loadPage();
      }
    } catch (error) {
      console.error("Failed to delete entry:", error);
      loadPage();
    }
  };

  const retryHistoryEntry = async (id: number) => {
    const result = await commands.retryHistoryEntryTranscription(id);
    if (result.status !== "ok") {
      throw new Error(String(result.error));
    }
  };

  const openRecordingsFolder = async () => {
    try {
      const result = await commands.openRecordingsFolder();
      if (result.status !== "ok") {
        throw new Error(String(result.error));
      }
    } catch (error) {
      console.error("Failed to open recordings folder:", error);
    }
  };

  // Create experiment for a saved recording
  const createExperiment = async (entryId: number) => {
    try {
      const result = await commands.createExperimentGroup(entryId);
      if (result.status === "ok") {
        setExperiments(prev => {
          const next = new Map(prev);
          next.set(entryId, result.data);
          return next;
        });
        toast.success("Experiment created");
      }
    } catch (e) {
      console.error("Failed to create experiment:", e);
      toast.error("Failed to create experiment");
    }
  };

  // Update ground truth
  const updateGroundTruth = async (entryId: number, groundTruth: string) => {
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
        setExperiments(prev => {
          const next = new Map(prev);
          next.set(entryId, result.data);
          return next;
        });
      }
    } catch (e) {
      console.error("Failed to update ground truth:", e);
    }
  };

  // Generate variants for all saved recordings
  const generateVariantsForAll = async () => {
    const savedEntries = entries.filter(e => e.saved);
    if (savedEntries.length === 0) {
      toast.error("No saved recordings");
      return;
    }

    const modelIds = availableModels.length > 0
      ? availableModels.slice(0, 5).map(m => m.id)
      : ["turbo", "medium", "small"];

    setGeneratingVariants(new Set(savedEntries.map(e => e.id)));

    let successCount = 0;
    for (const entry of savedEntries) {
      const exp = experiments.get(entry.id);
      if (!exp || !exp.ground_truth) continue;

      try {
        const result = await commands.generateVariants(exp.id, modelIds);
        if (result.status === "ok") {
          // Add variants to database
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
            setVariants(prev => {
              const next = new Map(prev);
              next.set(entry.id, varResult.data);
              return next;
            });
          }
          successCount++;
        }
      } catch (e) {
        console.error("Failed to generate variants for", entry.id, e);
      }
    }

    setGeneratingVariants(new Set());
    toast.success(`Generated variants for ${successCount}/${savedEntries.length} recordings`);
  };

  let content: React.ReactNode;

  if (loading) {
    content = (
      <div className="px-4 py-3 text-center text-text/60">
        {t("settings.history.loading")}
      </div>
    );
  } else if (entries.length === 0) {
    content = (
      <div className="px-4 py-3 text-center text-text/60">
        {t("settings.history.empty")}
      </div>
    );
  } else if (filteredEntries.length === 0) {
    content = (
      <div className="px-4 py-3 text-center text-text/60">
        {t("settings.history.noResults")}
      </div>
    );
  } else {
    content = (
      <>
        <div className="divide-y divide-mid-gray/20">
          {filteredEntries.map((entry) => (
            <HistoryEntryComponent
              key={entry.id}
              entry={entry}
              onToggleSaved={() => toggleSaved(entry.id)}
              onCopyText={() => copyToClipboard(entry.transcription_text)}
              getAudioUrl={getAudioUrl}
              deleteAudio={deleteAudioEntry}
              retryTranscription={retryHistoryEntry}
              experimentGroup={experiments.get(entry.id) || null}
              variants={variants.get(entry.id) || []}
              onCreateExperiment={() => createExperiment(entry.id)}
              onUpdateGroundTruth={(text) => updateGroundTruth(entry.id, text)}
            />
          ))}
        </div>
        {/* Sentinel for infinite scroll - only show when not searching */}
        {searchQuery.trim() === "" && <div ref={sentinelRef} className="h-1" />}
      </>
    );
  }

  return (
    <div className="max-w-3xl w-full mx-auto space-y-6">
      <div className="space-y-2">
        <div className="px-4 flex items-center justify-between">
          <div>
            <h2 className="text-xs font-medium text-mid-gray uppercase tracking-wide">
              {t("settings.history.title")}
            </h2>
          </div>
          <div className="flex items-center gap-2">
            {entries.some(e => e.saved && experiments.get(e.id)?.ground_truth) && (
              <Button
                variant="secondary"
                size="sm"
                onClick={generateVariantsForAll}
                disabled={generatingVariants.size > 0}
              >
                {generatingVariants.size > 0 ? (
                  <>
                    <Loader2 className="w-4 h-4 mr-1 animate-spin" />
                    Generating...
                  </>
                ) : (
                  <>
                    <Play className="w-4 h-4 mr-1" />
                    Generate Variants
                  </>
                )}
              </Button>
            )}
            <OpenRecordingsButton
              onClick={openRecordingsFolder}
              label={t("settings.history.openFolder")}
            />
          </div>
        </div>
        {/* Search bar */}
        <div className="px-4">
          <div className="relative">
            <Search className="absolute left-3 top-1/2 -translate-y-1/2 w-4 h-4 text-text/40" />
            <Input
              type="text"
              placeholder={t("settings.history.search")}
              value={searchQuery}
              onChange={(e) => setSearchQuery(e.target.value)}
              className="w-full pl-9"
            />
          </div>
        </div>
        <div className="bg-background border border-mid-gray/20 rounded-lg overflow-visible">
          {content}
        </div>
      </div>
    </div>
  );
};

interface HistoryEntryProps {
  entry: HistoryEntry;
  onToggleSaved: () => void;
  onCopyText: () => void;
  getAudioUrl: (fileName: string) => Promise<string | null>;
  deleteAudio: (id: number) => Promise<void>;
  retryTranscription: (id: number) => Promise<void>;
  experimentGroup: ExperimentGroup | null;
  variants: TranscriptionVariant[];
  onCreateExperiment: () => void;
  onUpdateGroundTruth: (text: string) => void;
}

const HistoryEntryComponent: React.FC<HistoryEntryProps> = ({
  entry,
  onToggleSaved,
  onCopyText,
  getAudioUrl,
  deleteAudio,
  retryTranscription,
  experimentGroup,
  variants,
  onCreateExperiment,
  onUpdateGroundTruth,
}) => {
  const { t, i18n } = useTranslation();
  const [showCopied, setShowCopied] = useState(false);
  const [retrying, setRetrying] = useState(false);
  const [editingGroundTruth, setEditingGroundTruth] = useState(false);
  const [groundTruth, setGroundTruth] = useState(
    experimentGroup?.ground_truth || entry.transcription_text,
  );

  const hasTranscription = entry.transcription_text.trim().length > 0;

  const handleLoadAudio = useCallback(
    () => getAudioUrl(entry.file_name),
    [getAudioUrl, entry.file_name],
  );

  const handleCopyText = () => {
    if (!hasTranscription) {
      return;
    }

    onCopyText();
    setShowCopied(true);
    setTimeout(() => setShowCopied(false), 2000);
  };

  const handleDeleteEntry = async () => {
    try {
      await deleteAudio(entry.id);
    } catch (error) {
      console.error("Failed to delete entry:", error);
      toast.error(t("settings.history.deleteError"));
    }
  };

  const handleRetranscribe = async () => {
    try {
      setRetrying(true);
      await retryTranscription(entry.id);
    } catch (error) {
      console.error("Failed to re-transcribe:", error);
      toast.error(t("settings.history.retranscribeError"));
    } finally {
      setRetrying(false);
    }
  };

  const formattedDate = formatDateTime(String(entry.timestamp), i18n.language);

  // Parse routing result JSON if present
  const routingResult = entry.routing_result
    ? (JSON.parse(entry.routing_result) as Array<{
        status: string;
        handler: string;
        classification: string;
        file_path: string | null;
      }>)
    : null;

  // Calculate match score helper
  const calculateMatchScore = (text: string, truth: string): number => {
    const a = text.toLowerCase().trim();
    const b = truth.toLowerCase().trim();
    if (a === b) return 100;

    const wordsA = a.split(/\s+/);
    const wordsB = b.split(/\s+/);
    const common = wordsA.filter((w) => wordsB.includes(w)).length;
    const total = Math.max(wordsA.length, wordsB.length);
    return Math.round((common / total) * 100);
  };

  return (
    <div className="px-4 py-2 pb-5 flex flex-col gap-3">
      <div className="flex justify-between items-center">
        <div className="flex items-center gap-2">
          <p className="text-sm font-medium">{formattedDate}</p>
          <span className="text-xs text-text/40 font-mono">#{entry.id}</span>
          {entry.saved && experimentGroup && (
            <Badge variant="success" className="text-xs">
              Experiment
            </Badge>
          )}
        </div>
        <div className="flex items-center gap-1">
          {/* Routing status tag */}
          {entry.routed && routingResult && (
            <Badge variant="success" className="mr-1">
              {t("settings.history.routed")}
            </Badge>
          )}
          <IconButton
            onClick={handleCopyText}
            disabled={!hasTranscription || retrying}
            title={t("settings.history.copyToClipboard")}
          >
            {showCopied ? (
              <Check width={16} height={16} />
            ) : (
              <Copy width={16} height={16} />
            )}
          </IconButton>
          <IconButton
            onClick={onToggleSaved}
            disabled={retrying}
            active={entry.saved}
            title={
              entry.saved
                ? t("settings.history.unsave")
                : t("settings.history.save")
            }
          >
            <Star
              width={16}
              height={16}
              fill={entry.saved ? "currentColor" : "none"}
            />
          </IconButton>
          <IconButton
            onClick={handleRetranscribe}
            disabled={retrying}
            title={t("settings.history.retranscribe")}
          >
            <RotateCcw
              width={16}
              height={16}
              style={
                retrying
                  ? { animation: "spin 1s linear infinite reverse" }
                  : undefined
              }
            />
          </IconButton>
          <IconButton
            onClick={handleDeleteEntry}
            disabled={retrying}
            title={t("settings.history.delete")}
          >
            <Trash2 width={16} height={16} />
          </IconButton>
        </div>
      </div>

      {/* Transcription text or ground truth editing */}
      {entry.saved && experimentGroup ? (
        <div className="space-y-2">
          <div className="flex items-center gap-2">
            <span className="text-xs text-primary font-medium">Ground Truth:</span>
            {editingGroundTruth ? (
              <div className="flex-1 flex items-center gap-2">
                <input
                  type="text"
                  className="flex-1 px-2 py-1 border border-border rounded bg-surface text-text text-sm"
                  value={groundTruth}
                  onChange={(e) => setGroundTruth(e.target.value)}
                  placeholder="Correct the transcription..."
                />
                <Button
                  size="sm"
                  onClick={() => {
                    onUpdateGroundTruth(groundTruth);
                    setEditingGroundTruth(false);
                  }}
                >
                  <Check className="w-3 h-3" />
                </Button>
                <Button
                  size="sm"
                  variant="secondary"
                  onClick={() => {
                    setGroundTruth(experimentGroup?.ground_truth || entry.transcription_text);
                    setEditingGroundTruth(false);
                  }}
                >
                  <X className="w-3 h-3" />
                </Button>
              </div>
            ) : (
              <div
                className="flex-1 cursor-pointer"
                onClick={() => setEditingGroundTruth(true)}
              >
                <p className="text-sm text-text">
                  {experimentGroup.ground_truth || entry.transcription_text}
                </p>
                <p className="text-xs text-text/40">Click to edit</p>
              </div>
            )}
          </div>

          {/* Variants */}
          {variants.length > 0 && (
            <div className="mt-2 space-y-1">
              <p className="text-xs text-text/60">Variants:</p>
              {variants
                .sort((a, b) => (a.ranking || 999) - (b.ranking || 999))
                .slice(0, 3)
                .map((variant) => {
                  const score = experimentGroup.ground_truth
                    ? calculateMatchScore(variant.transcription_text, experimentGroup.ground_truth)
                    : null;
                  return (
                    <div
                      key={variant.id}
                      className="flex items-center gap-2 text-xs"
                    >
                      <span className="font-mono text-text/60 w-20 truncate">
                        {variant.model_id}
                      </span>
                      {score !== null && (
                        <span className={`px-1.5 py-0.5 rounded text-xs ${
                          score >= 90 ? "bg-green-500/20 text-green-500" :
                          score >= 70 ? "bg-yellow-500/20 text-yellow-500" :
                          "bg-red-500/20 text-red-500"
                        }`}>
                          {score}%
                        </span>
                      )}
                      {variant.is_acceptable && (
                        <Check className="w-3 h-3 text-green-500" />
                      )}
                      <span className="flex-1 truncate text-text/80">
                        {variant.transcription_text}
                      </span>
                    </div>
                  );
                })}
              {variants.length > 3 && (
                <p className="text-xs text-text/40">+{variants.length - 3} more variants</p>
              )}
            </div>
          )}
        </div>
      ) : (
        <p
          className={`italic text-sm pb-2 ${
            retrying
              ? ""
              : hasTranscription
                ? "text-text/90 select-text cursor-text whitespace-pre-wrap break-words"
                : "text-text/40"
          }`}
          style={
            retrying
              ? { animation: "transcribe-pulse 3s ease-in-out infinite" }
              : undefined
          }
        >
          {retrying && (
            <style>{`
              @keyframes transcribe-pulse {
                0%, 100% { color: color-mix(in srgb, var(--color-text) 40%, transparent); }
                50% { color: color-mix(in srgb, var(--color-text) 90%, transparent); }
              }
            `}</style>
          )}
          {retrying
            ? t("settings.history.transcribing")
            : hasTranscription
              ? entry.transcription_text
              : t("settings.history.transcriptionFailed")}
        </p>
      )}

      {/* Create experiment button for saved entries without experiments */}
      {entry.saved && !experimentGroup && (
        <Button
          variant="secondary"
          size="sm"
          onClick={onCreateExperiment}
          className="w-full"
        >
          Create Experiment
        </Button>
      )}

      {/* Routing result details */}
      {entry.routed && routingResult && routingResult.length > 0 && (
        <div className="flex flex-wrap gap-1">
          {routingResult.map((result, idx) => (
            <Badge key={idx} variant="secondary" className="text-xs">
              {result.status} {result.handler}
              {result.classification && ` → ${result.classification}`}
            </Badge>
          ))}
        </div>
      )}

      <AudioPlayer onLoadRequest={handleLoadAudio} className="w-full" />
    </div>
  );
};
