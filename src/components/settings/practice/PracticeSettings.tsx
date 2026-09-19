import React, { useCallback, useEffect, useRef, useState } from "react";
import { convertFileSrc } from "@tauri-apps/api/core";
import { readFile } from "@tauri-apps/plugin-fs";
import {
  AlertCircle,
  AlertTriangle,
  CheckCircle,
  type LucideIcon,
  Mic,
  RotateCcw,
  Square,
  Trash2,
  X,
} from "lucide-react";
import { useTranslation } from "react-i18next";
import { toast } from "sonner";
import {
  commands,
  type DeliveryMetrics,
  type Level,
  type PaceRange,
  type PracticeSession,
} from "@/bindings";
import { useOsType } from "@/hooks/useOsType";
import { formatDateTime } from "@/utils/dateFormat";
import { MarkdownContent } from "../../whats-new/MarkdownContent";
import { Alert } from "../../ui/Alert";
import { AudioPlayer } from "../../ui/AudioPlayer";
import { Button } from "../../ui/Button";
import { PracticeTrends } from "./PracticeTrends";

// Keeps the final transcription under the ~24 minute limit of the Parakeet
// models, and a forgotten recording from growing without bound.
const MAX_RECORDING_SECONDS = 20 * 60;

type Phase = "idle" | "recording" | "analyzing" | "coaching";

// Outlives the component: switching sidebar tabs unmounts this page while the
// backend keeps recording, and coming back must find that recording again.
let recordingStartedAt: number | null = null;

const clock = (seconds: number) =>
  `${Math.floor(seconds / 60)}:${String(Math.floor(seconds % 60)).padStart(2, "0")}`;

export const PracticeSettings: React.FC = () => {
  const { t, i18n } = useTranslation();
  const osType = useOsType();
  const [phase, setPhase] = useState<Phase>(
    recordingStartedAt === null ? "idle" : "recording",
  );
  const [elapsed, setElapsed] = useState(0);
  const [sessions, setSessions] = useState<PracticeSession[]>([]);
  const [current, setCurrent] = useState<PracticeSession | null>(null);
  const [paceRange, setPaceRange] = useState<PaceRange | null>(null);
  // Bumped to abandon an in-flight coaching call: the backend still finishes and
  // stores its answer, the page just stops waiting for it.
  const coachingRun = useRef(0);

  const upsert = useCallback((session: PracticeSession) => {
    setSessions((all) => [
      session,
      ...all.filter((s) => s.entry.id !== session.entry.id),
    ]);
    setCurrent((shown) =>
      shown?.entry.id === session.entry.id ? session : shown,
    );
  }, []);

  useEffect(() => {
    commands.getPracticePaceRange().then(setPaceRange);
    commands.getPracticeSessions().then((result) => {
      if (result.status === "ok") setSessions(result.data);
      else toast.error(result.error);
    });
  }, []);

  const coach = useCallback(
    async (id: number) => {
      const run = ++coachingRun.current;
      setPhase("coaching");
      const result = await commands.coachPracticeSession(id);
      if (result.status === "ok") upsert(result.data);
      else toast.error(result.error);
      if (coachingRun.current === run) setPhase("idle");
    },
    [upsert],
  );

  const stop = useCallback(async () => {
    recordingStartedAt = null;
    setPhase("analyzing");
    const result = await commands.stopPractice();
    if (result.status === "error") {
      toast.error(result.error);
      setPhase("idle");
      return;
    }
    setCurrent(result.data);
    upsert(result.data);
    await coach(result.data.entry.id);
  }, [coach, upsert]);

  useEffect(() => {
    if (phase !== "recording") return;
    const startedAt = (recordingStartedAt ??= Date.now());
    const timer = setInterval(() => {
      const seconds = (Date.now() - startedAt) / 1000;
      setElapsed(seconds);
      if (seconds >= MAX_RECORDING_SECONDS) stop();
    }, 250);
    return () => clearInterval(timer);
  }, [phase, stop]);

  const start = async () => {
    const result = await commands.startPractice();
    if (result.status === "error") {
      toast.error(result.error);
      return;
    }
    setElapsed(0);
    setCurrent(null);
    setPhase("recording");
  };

  const cancel = async () => {
    if (phase === "recording") await commands.cancelPractice();
    recordingStartedAt = null;
    coachingRun.current++;
    setPhase("idle");
  };

  const remove = async (id: number) => {
    const result = await commands.deleteHistoryEntry(id);
    if (result.status === "error") {
      toast.error(result.error);
      return;
    }
    setSessions((all) => all.filter((s) => s.entry.id !== id));
    setCurrent((shown) => (shown?.entry.id === id ? null : shown));
  };

  const getAudioUrl = useCallback(
    async (fileName: string) => {
      const result = await commands.getAudioFilePath(fileName);
      if (result.status !== "ok") return null;
      if (osType === "linux") {
        const blob = new Blob([await readFile(result.data)], {
          type: "audio/wav",
        });
        return URL.createObjectURL(blob);
      }
      return convertFileSrc(result.data, "asset");
    },
    [osType],
  );

  const busy = phase === "analyzing" || phase === "coaching";

  return (
    <div className="max-w-3xl w-full mx-auto space-y-6">
      <div className="space-y-2">
        <h2 className="px-4 text-xs font-medium text-mid-gray uppercase tracking-wide">
          {t("practice.title")}
        </h2>
        <div className="bg-background border border-mid-gray/20 rounded-lg p-4 flex items-center gap-3">
          {phase === "recording" ? (
            <Button
              variant="danger"
              onClick={stop}
              className="flex items-center gap-2"
            >
              <Square width={14} height={14} />
              {t("practice.stop")}
            </Button>
          ) : (
            <Button
              onClick={start}
              disabled={busy}
              className="flex items-center gap-2"
            >
              <Mic width={14} height={14} />
              {t("practice.record")}
            </Button>
          )}
          <p className="text-sm text-text/70 flex-1">
            {phase === "idle" && t("practice.hint")}
            {phase === "recording" &&
              t("practice.recording", {
                elapsed: clock(elapsed),
                max: clock(MAX_RECORDING_SECONDS),
              })}
            {phase === "analyzing" && t("practice.analyzing")}
            {phase === "coaching" && t("practice.coaching")}
          </p>
          {(phase === "recording" || phase === "coaching") && (
            <Button
              variant="ghost"
              onClick={cancel}
              className="flex items-center gap-1"
            >
              <X width={14} height={14} />
              {t("practice.cancel")}
            </Button>
          )}
        </div>
      </div>

      {current && (
        <SessionDetail
          session={current}
          paceRange={paceRange}
          coaching={phase === "coaching"}
          onRetryCoaching={() => coach(current.entry.id)}
          getAudioUrl={getAudioUrl}
        />
      )}

      <PracticeTrends sessions={sessions} paceRange={paceRange} />

      <div className="space-y-2">
        <h2 className="px-4 text-xs font-medium text-mid-gray uppercase tracking-wide">
          {t("practice.pastSessions")}
        </h2>
        <div className="bg-background border border-mid-gray/20 rounded-lg divide-y divide-mid-gray/20">
          {sessions.length === 0 && (
            <div className="px-4 py-3 text-center text-text/60">
              {t("practice.empty")}
            </div>
          )}
          {sessions.map((session) => (
            <div key={session.entry.id} className="flex items-center">
              <button
                type="button"
                disabled={phase === "recording" || busy}
                onClick={() => setCurrent(session)}
                className={`flex-1 text-start px-4 py-3 cursor-pointer hover:bg-mid-gray/10 disabled:cursor-default disabled:opacity-60 ${
                  current?.entry.id === session.entry.id ? "bg-mid-gray/10" : ""
                }`}
              >
                <p className="text-sm font-medium">
                  {formatDateTime(
                    String(session.entry.timestamp),
                    i18n.language,
                  )}
                </p>
                <p className="text-xs text-text/60">{rowSummary(session, t)}</p>
              </button>
              <Button
                variant="danger-ghost"
                size="sm"
                onClick={() => remove(session.entry.id)}
                aria-label={t("practice.delete")}
                className="me-2"
              >
                <Trash2 width={14} height={14} />
              </Button>
            </div>
          ))}
        </div>
      </div>
    </div>
  );
};

type Translate = ReturnType<typeof useTranslation>["t"];

// A gate-rejected session has no trustworthy numbers; "0 WPM · 0 pauses" would
// read as a measurement.
const rowSummary = (session: PracticeSession, t: Translate) => {
  const count = (level: Level) =>
    session.findings.filter((f) => f.level === level).length;
  return session.data.metrics.quality?.is_reliable
    ? t("practice.glance", {
        workOn: count("work_on"),
        watch: count("watch"),
        good: count("good"),
      })
    : t("practice.notMeasured");
};

const totalFillers = (m: DeliveryMetrics) =>
  Object.values(m.filler_counts).reduce<number>((a, b) => a + (b ?? 0), 0);

interface SessionDetailProps {
  session: PracticeSession;
  paceRange: PaceRange | null;
  coaching: boolean;
  onRetryCoaching: () => void;
  getAudioUrl: (fileName: string) => Promise<string | null>;
}

const LEVEL_STYLE: Record<Level, { icon: LucideIcon; tone: string }> = {
  work_on: { icon: AlertCircle, tone: "text-red-400" },
  watch: { icon: AlertTriangle, tone: "text-yellow-500" },
  good: { icon: CheckCircle, tone: "text-green-500" },
};

const SessionDetail: React.FC<SessionDetailProps> = ({
  session,
  paceRange,
  coaching,
  onRetryCoaching,
  getAudioUrl,
}) => {
  const { t } = useTranslation();
  const { entry, data, findings } = session;
  const m = data.metrics;
  const fillers = totalFillers(m);
  const longPauses = m.pauses.filter((p) => p.duration_seconds >= 2).length;

  // Everything a finding's sentence might quote, in everyday units.
  const facts = {
    wpm: Math.round(m.words_per_minute),
    min: paceRange?.min_wpm ?? 0,
    max: paceRange?.max_wpm ?? 0,
    count: fillers,
    rate: m.fillers_per_minute.toFixed(1),
    longPauses,
    longest: m.longest_pause_seconds.toFixed(1),
  };
  // The big line on each card: a number where a number means something to a
  // person, a word where it would not (nobody thinks in Hz of deviation).
  const headline: Record<string, string> = {
    pace: t("practice.headline.pace", facts),
    fillers: t("practice.headline.fillers", facts),
    pauses: t("practice.headline.pauses", { count: longPauses }),
  };
  const count = (level: Level) =>
    findings.filter((f) => f.level === level).length;

  return (
    <div className="space-y-4">
      {m.quality?.is_reliable ? (
        <div className="space-y-2">
          <p className="px-4 text-sm">
            {t("practice.glance", {
              workOn: count("work_on"),
              watch: count("watch"),
              good: count("good"),
            })}
          </p>
          <div className="grid grid-cols-2 gap-2">
            {findings.map((finding, index) => {
              const { icon: Icon, tone } = LEVEL_STYLE[finding.level];
              // An odd count would orphan the last card; widen the first — it is
              // the most urgent one — instead.
              const wide = index === 0 && findings.length % 2 === 1;
              return (
                <div
                  key={finding.metric}
                  className={`bg-background border border-mid-gray/20 rounded-lg p-3 ${wide ? "col-span-2" : ""}`}
                >
                  <div className="flex items-center justify-between">
                    <p className="text-xs text-mid-gray uppercase tracking-wide">
                      {t(`practice.metric.${finding.metric}`)}
                    </p>
                    <span
                      className={`flex items-center gap-1 text-xs font-medium ${tone}`}
                    >
                      <Icon width={14} height={14} />
                      {t(`practice.level.${finding.level}`)}
                    </span>
                  </div>
                  <p className="text-lg font-semibold">
                    {headline[finding.metric] ??
                      t(`practice.word.${finding.metric}.${finding.note}`)}
                  </p>
                  <p className="text-sm text-text/70">
                    {t(
                      `practice.finding.${finding.metric}.${finding.note}`,
                      facts,
                    )}
                  </p>
                </div>
              );
            })}
          </div>
        </div>
      ) : (
        <Alert variant="warning">
          {t("practice.unreliable", { reason: m.quality?.warning ?? "" })}
        </Alert>
      )}

      <div className="bg-background border border-mid-gray/20 rounded-lg p-4 space-y-3">
        {coaching ? (
          <p className="text-sm text-text/70">{t("practice.coaching")}</p>
        ) : data.coaching ? (
          <>
            <div className="select-text">
              <MarkdownContent markdown={data.coaching} />
            </div>
            {data.coaching_model && (
              <p className="text-xs text-text/50">
                {t("practice.coachedBy", { model: data.coaching_model })}
              </p>
            )}
          </>
        ) : (
          <div className="flex items-center gap-3">
            <p className="text-sm text-text/70 flex-1">
              {data.coaching_error ?? t("practice.noCoaching")}
            </p>
            <Button
              variant="secondary"
              size="sm"
              onClick={onRetryCoaching}
              className="flex items-center gap-1"
            >
              <RotateCcw width={14} height={14} />
              {t("practice.retryCoaching")}
            </Button>
          </div>
        )}
      </div>

      <div className="bg-background border border-mid-gray/20 rounded-lg p-4 space-y-3">
        <AudioPlayer
          key={entry.file_name}
          onLoadRequest={() => getAudioUrl(entry.file_name)}
          className="w-full"
        />
        <details open={fillers > 0}>
          <summary className="text-sm cursor-pointer text-text/70">
            {t("practice.transcript")}
            {fillers > 0 && (
              <span className="ms-2 text-xs text-text/50">
                {t("practice.fillersHighlighted", { count: fillers })}
              </span>
            )}
          </summary>
          <p className="text-sm pt-2 select-text whitespace-pre-wrap">
            <HighlightedTranscript
              text={entry.transcription_text}
              fillerWords={Object.keys(m.filler_counts)}
            />
          </p>
        </details>
      </div>

      {m.quality?.is_reliable && (
        <details>
          <summary className="px-4 text-sm cursor-pointer text-text/70">
            {t("practice.details.title")}
          </summary>
          <div className="space-y-2 pt-2">
            <Contour
              label={t("practice.chart.pitch")}
              values={m.pitch_contour}
              metrics={m}
              gapsAtZero
            />
            <Contour
              label={t("practice.chart.volume")}
              values={m.energy_contour}
              metrics={m}
            />
            <p className="px-4 text-xs text-text/60 select-text">
              {t("practice.details.numbers", {
                articulation: Math.round(m.articulation_rate),
                voiced: Math.round(m.speaking_ratio * 100),
                pauses: m.pauses.length,
                pitch: Math.round(m.mean_pitch_hz),
                pitchSd: Math.round(m.pitch_std_dev_hz),
                range: Math.round(m.dynamic_range_db),
              })}
            </p>
          </div>
        </details>
      )}
    </div>
  );
};

// Mirrors count_fillers in src-tauri/src/audio_toolkit/analysis.rs: a token is
// a filler when, lowercased and with edge punctuation trimmed, it is on the list.
// Highlighting by that same rule means the marks always add up to the count.
const normalizeToken = (token: string) =>
  token.replace(/^[^\p{L}\p{N}]+|[^\p{L}\p{N}]+$/gu, "").toLowerCase();

const HighlightedTranscript: React.FC<{
  text: string;
  fillerWords: string[];
}> = ({ text, fillerWords }) => {
  const fillers = new Set(fillerWords);
  // The capture group keeps the whitespace runs, so the text re-joins exactly.
  return (
    <>
      {text.split(/(\s+)/).map((token, i) =>
        fillers.has(normalizeToken(token)) ? (
          <mark
            key={i}
            className="rounded px-0.5 bg-yellow-500/30 text-inherit"
          >
            {token}
          </mark>
        ) : (
          token
        ),
      )}
    </>
  );
};

interface ContourProps {
  label: string;
  values: number[];
  metrics: DeliveryMetrics;
  /** Pitch: a 0 is an unvoiced gap, so the line breaks there instead of diving. */
  gapsAtZero?: boolean;
}

// ponytail: hand-rolled SVG, no chart dependency. Swap for a chart library when
// trend charts across sessions arrive and axes/tooltips start to matter.
const Contour: React.FC<ContourProps> = ({
  label,
  values,
  metrics,
  gapsAtZero = false,
}) => {
  const drawn = gapsAtZero ? values.filter((v) => v > 0) : values;
  if (drawn.length < 2) return null;

  const min = Math.min(...drawn);
  const span = Math.max(...drawn) - min || 1;
  const width = values.length - 1;
  const interval = metrics.contour_interval_seconds;

  // One path, with a fresh "M" after every gap.
  let path = "";
  let penDown = false;
  values.forEach((v, i) => {
    if (gapsAtZero && v <= 0) {
      penDown = false;
      return;
    }
    const y = 100 - ((v - min) / span) * 100;
    path += `${penDown ? "L" : "M"}${i},${y.toFixed(1)} `;
    penDown = true;
  });

  return (
    <div className="bg-background border border-mid-gray/20 rounded-lg p-3">
      <div className="flex justify-between text-xs text-mid-gray pb-1">
        <span className="uppercase tracking-wide">{label}</span>
        <span>{clock(metrics.duration_seconds)}</span>
      </div>
      <svg
        viewBox={`0 0 ${width} 100`}
        preserveAspectRatio="none"
        className="w-full h-20 text-logo-primary"
      >
        {interval > 0 &&
          metrics.pauses.map((pause) => (
            <rect
              key={pause.start_seconds}
              x={pause.start_seconds / interval}
              width={pause.duration_seconds / interval}
              y={0}
              height={100}
              className="fill-mid-gray/20"
            />
          ))}
        <path
          d={path}
          fill="none"
          stroke="currentColor"
          strokeWidth={1.5}
          vectorEffect="non-scaling-stroke"
        />
      </svg>
    </div>
  );
};
