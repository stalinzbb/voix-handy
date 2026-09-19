import React from "react";
import { useTranslation } from "react-i18next";
import type { PaceRange, PracticeSession } from "@/bindings";
import { formatDateTime } from "@/utils/dateFormat";

const WIDTH = 300;
const HEIGHT = 84;
const PAD = 8;

interface TrendProps {
  label: string;
  /** Oldest first. */
  points: { value: number; when: string }[];
  format: (value: number) => string;
  /** Drawn as a neutral band behind the line, e.g. the target pace. */
  band?: [number, number];
}

// One measure per tile, each on its own axis — four measures with four different
// units never share a y-scale. Single series, so the title is the legend.
//
// ponytail: native <title> tooltips on enlarged hit targets rather than a custom
// hover layer. Build a real crosshair tooltip if these charts grow axes.
const Trend: React.FC<TrendProps> = ({ label, points, format, band }) => {
  const { t } = useTranslation();
  const values = points.map((p) => p.value);
  const low = Math.min(...values, ...(band ?? []));
  const high = Math.max(...values, ...(band ?? []));
  const span = high - low || 1;
  const x = (i: number) =>
    PAD + (i * (WIDTH - 2 * PAD)) / Math.max(points.length - 1, 1);
  const y = (v: number) =>
    HEIGHT - PAD - ((v - low) / span) * (HEIGHT - 2 * PAD);

  const latest = values[values.length - 1];
  const change = latest - values[values.length - 2];

  return (
    <div className="bg-background border border-mid-gray/20 rounded-lg p-3">
      <p className="text-xs text-mid-gray uppercase tracking-wide">{label}</p>
      <p className="text-xl font-semibold">{format(latest)}</p>
      <p className="text-xs text-text/60">
        {t("practice.trends.change", {
          change: `${change >= 0 ? "+" : "−"}${format(Math.abs(change))}`,
        })}
      </p>
      <svg
        viewBox={`0 0 ${WIDTH} ${HEIGHT}`}
        className="w-full text-logo-primary mt-1"
        role="img"
        aria-label={label}
      >
        {band && (
          <rect
            x={0}
            width={WIDTH}
            y={y(band[1])}
            height={y(band[0]) - y(band[1])}
            className="fill-mid-gray/15"
          />
        )}
        <polyline
          points={points.map((p, i) => `${x(i)},${y(p.value)}`).join(" ")}
          fill="none"
          stroke="currentColor"
          strokeWidth={2}
          strokeLinejoin="round"
          strokeLinecap="round"
        />
        {points.map((p, i) => (
          <g key={p.when + i}>
            <circle cx={x(i)} cy={y(p.value)} r={4} fill="currentColor" />
            {/* Hit target larger than the mark */}
            <circle cx={x(i)} cy={y(p.value)} r={12} fill="transparent">
              <title>{`${p.when} · ${format(p.value)}`}</title>
            </circle>
          </g>
        ))}
      </svg>
    </div>
  );
};

interface PracticeTrendsProps {
  /** Newest first, as the backend returns them. */
  sessions: PracticeSession[];
  paceRange: PaceRange | null;
}

export const PracticeTrends: React.FC<PracticeTrendsProps> = ({
  sessions,
  paceRange,
}) => {
  const { t, i18n } = useTranslation();
  // Gate-rejected sessions have no trustworthy numbers; plotting their zeros
  // would draw a cliff that never happened.
  const measured = sessions
    .filter((s) => s.data.metrics.quality?.is_reliable)
    .reverse();
  if (measured.length < 2) return null;

  const series = (pick: (s: PracticeSession) => number) =>
    measured.map((s) => ({
      value: pick(s),
      when: formatDateTime(String(s.entry.timestamp), i18n.language),
    }));
  const round = (v: number) => String(Math.round(v));

  return (
    <div className="space-y-2">
      <h2 className="px-4 text-xs font-medium text-mid-gray uppercase tracking-wide">
        {t("practice.trends.title", { count: measured.length })}
      </h2>
      <div className="grid grid-cols-2 gap-2">
        <Trend
          label={t("practice.metric.pace")}
          points={series((s) => s.data.metrics.words_per_minute)}
          format={(v) => t("practice.unit.wpm", { value: round(v) })}
          band={paceRange ? [paceRange.min_wpm, paceRange.max_wpm] : undefined}
        />
        <Trend
          label={t("practice.trends.fillerRate")}
          points={series((s) => s.data.metrics.fillers_per_minute)}
          format={(v) => t("practice.unit.perMinute", { value: v.toFixed(1) })}
        />
        <Trend
          label={t("practice.metric.pitch")}
          points={series((s) => s.data.metrics.pitch_std_dev_hz)}
          format={(v) => t("practice.unit.hz", { value: round(v) })}
        />
        <Trend
          label={t("practice.trends.longestPause")}
          points={series((s) => s.data.metrics.longest_pause_seconds)}
          format={(v) => t("practice.unit.seconds", { value: v.toFixed(1) })}
        />
      </div>
    </div>
  );
};
