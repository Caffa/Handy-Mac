import React, { useState, useEffect } from "react";
import { useTranslation } from "react-i18next";
import { commands, type HistoryEntry, type ModelInfo } from "@/bindings";
import { SettingsGroup } from "../../ui/SettingsGroup";
import { Button } from "../../ui/Button";
import { RotateCcw, Download, Tag, FolderOpen } from "lucide-react";
import { save } from "@tauri-apps/plugin-dialog";
import { writeFile } from "@tauri-apps/plugin-fs";
import { convertFileSrc } from "@tauri-apps/api/core";

interface TestResult {
  modelId: string;
  modelName: string;
  text: string;
  modelIdUsed?: string;
  suppressedTokenCount?: number;
}

interface TagInputProps {
  entryId: number;
  currentTags: string[];
  onTagsUpdate: (id: number, tags: string[]) => void;
}

const TagInput: React.FC<TagInputProps> = ({
  entryId,
  currentTags,
  onTagsUpdate,
}) => {
  const [isEditing, setIsEditing] = useState(false);
  const [tagInput, setTagInput] = useState("");

  const commonTags = ["fast", "slow", "test", "good", "bad", "baseline"];

  const handleAddTag = (tag: string) => {
    const normalized = tag.toLowerCase().trim();
    if (normalized && !currentTags.includes(normalized)) {
      onTagsUpdate(entryId, [...currentTags, normalized]);
    }
  };

  const handleRemoveTag = (tag: string) => {
    onTagsUpdate(entryId, currentTags.filter((t) => t !== tag));
  };

  const handleCustomTag = () => {
    if (tagInput.trim()) {
      handleAddTag(tagInput);
      setTagInput("");
    }
  };

  return (
    <div className="flex flex-wrap gap-1 items-center">
      {currentTags.map((tag) => (
        <button
          key={tag}
          onClick={() => handleRemoveTag(tag)}
          className="px-2 py-0.5 text-xs bg-primary/20 text-primary rounded-full hover:bg-primary/30 transition-colors flex items-center gap-1"
          title="Click to remove"
        >
          {tag}
          <span className="text-primary/60">×</span>
        </button>
      ))}
      {isEditing ? (
        <div className="flex gap-1 items-center">
          <input
            type="text"
            value={tagInput}
            onChange={(e) => setTagInput(e.target.value)}
            onKeyDown={(e) => {
              if (e.key === "Enter") {
                handleCustomTag();
              }
            }}
            placeholder="Add tag..."
            className="px-2 py-0.5 text-xs border border-border rounded bg-surface text-text w-20"
            autoFocus
          />
          <button
            onClick={handleCustomTag}
            className="px-2 py-0.5 text-xs bg-primary/20 text-primary rounded hover:bg-primary/30"
          >
            Add
          </button>
        </div>
      ) : (
        <button
          onClick={() => setIsEditing(true)}
          className="px-2 py-0.5 text-xs border border-dashed border-border text-text/50 rounded hover:border-primary hover:text-primary transition-colors"
        >
          + tag
        </button>
      )}
      <div className="flex gap-1">
        {commonTags
          .filter((t) => !currentTags.includes(t))
          .slice(0, 3)
          .map((tag) => (
            <button
              key={tag}
              onClick={() => handleAddTag(tag)}
              className="px-2 py-0.5 text-xs text-text/40 hover:text-primary transition-colors"
            >
              +{tag}
            </button>
          ))}
      </div>
    </div>
  );
};

export const TranscriptionLab: React.FC = () => {
  const { t } = useTranslation();
  const [entries, setEntries] = useState<HistoryEntry[]>([]);
  const [selectedEntryId, setSelectedEntryId] = useState<number | null>(null);
  const [models, setModels] = useState<ModelInfo[]>([]);
  const [selectedModelId, setSelectedModelId] = useState<string>("");
  const [testing, setTesting] = useState(false);
  const [results, setResults] = useState<TestResult[]>([]);
  const [error, setError] = useState<string | null>(null);

  // Load history entries (saved ones first)
  useEffect(() => {
    const loadEntries = async () => {
      try {
        const result = await commands.getHistoryEntries(null, 50);
        if (result.status === "ok") {
          // Sort saved entries first
          const sorted = result.data.entries.sort((a, b) => {
            if (a.saved && !b.saved) return -1;
            if (!a.saved && b.saved) return 1;
            return b.timestamp - a.timestamp;
          });
          setEntries(sorted);
        }
      } catch (e) {
        console.error("Failed to load history:", e);
      }
    };
    loadEntries();
  }, []);

  // Load available models
  useEffect(() => {
    const loadModels = async () => {
      try {
        const result = await commands.getAvailableModels();
        if (result.status === "ok") {
          const downloaded = result.data.filter((m) => m.is_downloaded);
          setModels(downloaded);
          if (downloaded.length > 0) {
            setSelectedModelId(downloaded[0].id);
          }
        }
      } catch (e) {
        console.error("Failed to load models:", e);
      }
    };
    loadModels();
  }, []);

  const handleTagsUpdate = async (entryId: number, tags: string[]) => {
    try {
      const tagsJson = tags.length > 0 ? JSON.stringify(tags) : null;
      await commands.updateHistoryEntryTags(entryId, tagsJson);
      setEntries((prev) =>
        prev.map((e) =>
          e.id === entryId ? { ...e, tags: tagsJson } : e,
        ),
      );
    } catch (e) {
      console.error("Failed to update tags:", e);
    }
  };

  const handleDownloadAudio = async (entry: HistoryEntry) => {
    try {
      const filePath = await commands.getAudioFilePath(entry.file_name);
      if (filePath.status === "ok") {
        // Open the recordings folder in Finder/Explorer
        await commands.openPath(filePath.data.replace(/\/[^/]+$/, ""));
      }
    } catch (e) {
      console.error("Failed to open recordings folder:", e);
    }
  };

  const handleExportTagged = async () => {
    const taggedEntries = entries.filter((e) => e.tags);
    if (taggedEntries.length === 0) {
      setError("No tagged entries to export");
      return;
    }

    try {
      const filePath = await save({
        defaultPath: `tagged-recordings-${Date.now()}.json`,
        filters: [{ name: "JSON", extensions: ["json"] }],
      });

      if (!filePath) return;

      const exportData = await Promise.all(
        taggedEntries.map(async (entry) => {
          const audioPath = await commands.getAudioFilePath(entry.file_name);
          return {
            id: entry.id,
            timestamp: entry.timestamp,
            transcription: entry.transcription_text,
            model_id: entry.model_id,
            tags: JSON.parse(entry.tags || "[]"),
            audio_file: entry.file_name,
            saved: entry.saved,
          };
        }),
      );

      await writeFile(filePath, new TextEncoder().encode(JSON.stringify(exportData, null, 2)));
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    }
  };

  const runTest = async () => {
    if (!selectedEntryId || !selectedModelId) return;

    setTesting(true);
    setError(null);

    try {
      // Get current model
      const settingsResult = await commands.getAppSettings();
      if (settingsResult.status !== "ok") {
        throw new Error("Failed to get settings");
      }
      const originalModel = settingsResult.data.selected_model;

      // Switch to test model
      await commands.setActiveModel(selectedModelId);

      // Re-transcribe
      await commands.retryHistoryEntryTranscription(selectedEntryId);

      // Get updated entry
      const entryResult = await commands.getHistoryEntries(null, 50);
      if (entryResult.status === "ok") {
        const updated = entryResult.data.entries.find(
          (e) => e.id === selectedEntryId,
        );
        if (updated) {
          const modelInfo = models.find((m) => m.id === selectedModelId);
          setResults((prev) => [
            {
              modelId: selectedModelId,
              modelName: modelInfo?.name || selectedModelId,
              text: updated.transcription_text,
              modelIdUsed: updated.model_id || undefined,
            },
            ...prev.slice(0, 4), // Keep last 5 results
          ]);
        }
      }

      // Restore original model
      if (originalModel) {
        await commands.setActiveModel(originalModel);
      }
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    } finally {
      setTesting(false);
    }
  };

  return (
    <SettingsGroup title="Transcription Lab">
      <div className="space-y-4">
        <p className="text-sm text-text/70">
          Test recordings with different models and tag them for your agentic
          coding team to analyze. Mark recordings as "fast" or "slow" speech,
          then export the data for automated experimentation.
        </p>

        {/* Tagged entries count */}
        <div className="flex gap-2 items-center text-sm">
          <span className="text-text/60">
            {entries.filter((e) => e.tags).length} tagged recordings
          </span>
          {entries.filter((e) => e.tags).length > 0 && (
            <Button variant="secondary" size="sm" onClick={handleExportTagged}>
              <Download className="w-4 h-4 mr-1" />
              Export Tagged Data
            </Button>
          )}
        </div>

        {/* Entry selector with tags */}
        <div className="space-y-2">
          <label className="text-sm font-medium">Select Recording</label>
          <div className="space-y-2">
            {entries.slice(0, 10).map((entry) => (
              <div
                key={entry.id}
                className={`p-3 border rounded-lg cursor-pointer transition-colors ${
                  selectedEntryId === entry.id
                    ? "border-primary bg-primary/5"
                    : "border-border hover:border-primary/50"
                }`}
                onClick={() => setSelectedEntryId(entry.id)}
              >
                <div className="flex justify-between items-start">
                  <div className="flex-1 min-w-0">
                    <div className="flex items-center gap-2">
                      <span className="text-xs text-text/50">#{entry.id}</span>
                      <span className="text-xs text-text/40">
                        {new Date(entry.timestamp * 1000).toLocaleDateString()}
                      </span>
                      {entry.saved && <span className="text-xs">⭐</span>}
                    </div>
                    <p className="text-sm text-text truncate mt-1">
                      {entry.transcription_text.substring(0, 80)}
                      {entry.transcription_text.length > 80 ? "..." : ""}
                    </p>
                    <div className="mt-1">
                      <TagInput
                        entryId={entry.id}
                        currentTags={entry.tags ? JSON.parse(entry.tags) : []}
                        onTagsUpdate={handleTagsUpdate}
                      />
                    </div>
                  </div>
                  <button
                    onClick={(e) => {
                      e.stopPropagation();
                      handleDownloadAudio(entry);
                    }}
                    className="ml-2 p-1 text-text/40 hover:text-primary transition-colors"
                    title="Open audio file"
                  >
                    <Download className="w-4 h-4" />
                  </button>
                </div>
              </div>
            ))}
          </div>
        </div>

        {/* Model selector */}
        <div className="space-y-2">
          <label className="text-sm font-medium">Test with Model</label>
          <select
            className="w-full p-2 border border-border rounded bg-surface text-text"
            value={selectedModelId}
            onChange={(e) => setSelectedModelId(e.target.value)}
          >
            {models.map((model) => (
              <option key={model.id} value={model.id}>
                {model.name} {model.is_recommended ? "(Recommended)" : ""}
              </option>
            ))}
          </select>
        </div>

        {/* Test button */}
        <Button
          onClick={runTest}
          disabled={!selectedEntryId || !selectedModelId || testing}
          className="w-full"
        >
          {testing ? (
            <>
              <RotateCcw
                width={16}
                height={16}
                className="animate-spin mr-2"
              />
              Testing...
            </>
          ) : (
            "Run Test"
          )}
        </Button>

        {/* Error */}
        {error && (
          <div className="p-3 bg-red-500/10 border border-red-500/30 rounded text-sm text-red-600">
            {error}
          </div>
        )}

        {/* Results */}
        {results.length > 0 && (
          <div className="space-y-3">
            <h4 className="text-sm font-medium">Test Results</h4>
            {results.map((result, idx) => (
              <div
                key={idx}
                className="p-3 bg-surface-secondary border border-border rounded space-y-1"
              >
                <div className="flex justify-between items-center">
                  <span className="text-xs font-mono text-text/60">
                    {result.modelName}
                  </span>
                  {result.modelIdUsed && (
                    <span className="text-xs text-text/40">
                      Model ID: {result.modelIdUsed}
                    </span>
                  )}
                </div>
                <p className="text-sm text-text/90">{result.text}</p>
              </div>
            ))}
          </div>
        )}
      </div>
    </SettingsGroup>
  );
};