import React, { useState, useEffect, useCallback } from "react";
import { useTranslation } from "react-i18next";
import { commands } from "../../../bindings";
import { SettingsGroup } from "../../ui/SettingsGroup";
import { BarChart3, Clock, FileText, Zap, Flame } from "lucide-react";

interface UsageStats {
  total_transcriptions: number;
  total_words: number;
  today_transcriptions: number;
  today_words: number;
  estimated_minutes_saved: number;
  current_streak_days: number;
  longest_streak_days: number;
  daily_stats: [string, number, number][];
}

const StatCard: React.FC<{
  icon: React.ReactNode;
  label: string;
  value: string;
  sublabel?: string;
}> = ({ icon, label, value, sublabel }) => (
  <div className="flex items-center gap-3 p-3">
    <div className="flex items-center justify-center w-10 h-10 rounded-lg bg-logo-primary/10 text-logo-primary shrink-0">
      {icon}
    </div>
    <div className="flex flex-col min-w-0">
      <span className="text-2xl font-bold text-foreground leading-tight">
        {value}
      </span>
      <span className="text-xs text-muted-foreground leading-tight">
        {label}
      </span>
      {sublabel && (
        <span className="text-[10px] text-muted-foreground/60 leading-tight">
          {sublabel}
        </span>
      )}
    </div>
  </div>
);

const MiniBarChart: React.FC<{
  data: [string, number, number][];
}> = ({ data }) => {
  if (data.length === 0) {
    return (
      <div className="flex items-center justify-center h-24 text-xs text-muted-foreground/60">
        No activity yet
      </div>
    );
  }

  const maxWords = Math.max(...data.map((d) => d[2]), 1);

  return (
    <div className="flex items-end gap-[2px] h-24 px-1">
      {data.map(([date, count, words], i) => {
        const height = Math.max((words / maxWords) * 100, count > 0 ? 4 : 0);
        const isToday =
          date === new Date().toISOString().slice(0, 10);
        return (
          <div
            key={date}
            className="flex-1 flex flex-col items-center gap-1 group relative"
          >
            <div
              className={`w-full rounded-t-sm transition-all ${
                isToday ? "bg-logo-primary" : "bg-logo-primary/50"
              } hover:bg-logo-primary/80`}
              style={{ height: `${height}%`, minHeight: count > 0 ? "2px" : "0" }}
            />
            <div className="absolute -top-8 left-1/2 -translate-x-1/2 bg-background border border-mid-gray/20 rounded px-1.5 py-0.5 text-[10px] whitespace-nowrap opacity-0 group-hover:opacity-100 transition-opacity pointer-events-none z-10 shadow-sm">
              <span className="font-medium">{date.slice(5)}</span>
              <span className="text-muted-foreground ml-1">
                {count} rec{count !== 1 ? "s" : ""} · {words} w
              </span>
            </div>
          </div>
        );
      })}
    </div>
  );
};

export const UsageSettings: React.FC = () => {
  const { t } = useTranslation();
  const [stats, setStats] = useState<UsageStats | null>(null);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);

  const fetchStats = useCallback(async () => {
    try {
      setLoading(true);
      setError(null);
      const result = await commands.getUsageStats();
      if (result.status === "ok") {
        setStats(result.data);
      } else {
        setError(result.error);
      }
    } catch (e) {
      setError(String(e));
    } finally {
      setLoading(false);
    }
  }, []);

  useEffect(() => {
    fetchStats();
  }, [fetchStats]);

  if (loading) {
    return (
      <div className="max-w-3xl w-full mx-auto space-y-6">
        <SettingsGroup title={t("settings.usage.title")}>
          <div className="flex items-center justify-center py-12 text-sm text-muted-foreground">
            Loading usage data...
          </div>
        </SettingsGroup>
      </div>
    );
  }

  if (error) {
    return (
      <div className="max-w-3xl w-full mx-auto space-y-6">
        <SettingsGroup title={t("settings.usage.title")}>
          <div className="flex items-center justify-center py-12 text-sm text-destructive">
            Failed to load usage data: {error}
          </div>
        </SettingsGroup>
      </div>
    );
  }

  if (!stats) return null;

  const formatMinutes = (minutes: number): string => {
    if (minutes < 1) return "<1 min";
    if (minutes < 60) return `${Math.round(minutes)} min`;
    const hours = Math.floor(minutes / 60);
    const mins = Math.round(minutes % 60);
    return mins > 0 ? `${hours}h ${mins}m` : `${hours}h`;
  };

  const formatWords = (words: number): string => {
    if (words >= 1000) return `${(words / 1000).toFixed(1)}k`;
    return String(words);
  };

  return (
    <div className="max-w-3xl w-full mx-auto space-y-6">
      <SettingsGroup title={t("settings.usage.title")}>
        <div className="grid grid-cols-2 gap-0 divide-x divide-mid-gray/20">
          <StatCard
            icon={<FileText size={20} />}
            label={t("settings.usage.totalTranscriptions")}
            value={String(stats.total_transcriptions)}
          />
          <StatCard
            icon={<BarChart3 size={20} />}
            label={t("settings.usage.totalWords")}
            value={formatWords(stats.total_words)}
          />
          <StatCard
            icon={<Clock size={20} />}
            label={t("settings.usage.timeSaved")}
            value={formatMinutes(stats.estimated_minutes_saved)}
            sublabel={t("settings.usage.timeSavedSub")}
          />
          <StatCard
            icon={<Flame size={20} />}
            label={t("settings.usage.currentStreak")}
            value={`${stats.current_streak_days}`}
            sublabel={
              stats.longest_streak_days > 0
                ? `${t("settings.usage.bestStreak")}: ${stats.longest_streak_days}`
                : undefined
            }
          />
        </div>
      </SettingsGroup>

      <SettingsGroup title={t("settings.usage.today")}>
        <div className="p-3 space-y-1">
          <div className="flex justify-between text-sm">
            <span className="text-muted-foreground">
              {t("settings.usage.transcriptions")}
            </span>
            <span className="font-medium">{stats.today_transcriptions}</span>
          </div>
          <div className="flex justify-between text-sm">
            <span className="text-muted-foreground">
              {t("settings.usage.words")}
            </span>
            <span className="font-medium">{formatWords(stats.today_words)}</span>
          </div>
        </div>
      </SettingsGroup>

      <SettingsGroup title={t("settings.usage.last30Days")}>
        <div className="p-3">
          <MiniBarChart data={stats.daily_stats} />
        </div>
      </SettingsGroup>
    </div>
  );
};
