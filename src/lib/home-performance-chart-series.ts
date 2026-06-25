import type { ProfilePerformancePoint, ProfilePerformanceRange } from "./platform-api";
import type { HomePerformanceDailyStat } from "./home-performance-snapshot";

export type PerformanceChartSeriesKey = "pnl" | "volume" | "maker" | "lp";

export const PERFORMANCE_CHART_SERIES: Array<{
  key: PerformanceChartSeriesKey;
  label: string;
  color: string;
}> = [
  { key: "pnl", label: "PnL", color: "var(--green)" },
  { key: "volume", label: "Volume", color: "#f0a030" },
  { key: "maker", label: "Maker", color: "var(--accent)" },
  { key: "lp", label: "LP Rewards", color: "#b692f0" },
];

export function getPerformanceRangeStartMs(range: ProfilePerformanceRange, asOf: Date): number {
  const ms = asOf.getTime();
  switch (range) {
    case "6h":
      return ms - 6 * 60 * 60 * 1000;
    case "1d":
      return ms - 24 * 60 * 60 * 1000;
    case "7d":
      return ms - 7 * 24 * 60 * 60 * 1000;
    case "30d":
      return ms - 30 * 24 * 60 * 60 * 1000;
    default:
      return 0;
  }
}

function isDailyStatInRange(date: string, rangeStartMs: number): boolean {
  const dayEnd = Date.parse(`${date}T23:59:59.999Z`);
  return Number.isFinite(dayEnd) && dayEnd >= rangeStartMs;
}

export function filterSeriesByRange(
  points: ProfilePerformancePoint[],
  rangeStartMs: number,
): ProfilePerformancePoint[] {
  return points.filter((point) => {
    const ts = Date.parse(point.ts);
    return Number.isFinite(ts) && ts >= rangeStartMs;
  });
}

export function buildCumulativeDailySeries(
  stats: HomePerformanceDailyStat[],
  rangeStartMs: number,
  valueOf: (stat: HomePerformanceDailyStat) => number | null,
): ProfilePerformancePoint[] {
  const sorted = [...stats].sort((left, right) => left.date.localeCompare(right.date));
  let cumulative = 0;
  const points: ProfilePerformancePoint[] = [];

  for (const stat of sorted) {
    if (!isDailyStatInRange(stat.date, rangeStartMs)) {
      continue;
    }
    cumulative += valueOf(stat) ?? 0;
    points.push({
      ts: `${stat.date}T12:00:00.000Z`,
      value: cumulative,
      raw_value: cumulative,
    });
  }

  return points;
}

export function sumDailyStatInRange(
  stats: HomePerformanceDailyStat[],
  rangeStartMs: number,
  valueOf: (stat: HomePerformanceDailyStat) => number | null,
): number | null {
  let total = 0;
  let seen = false;

  for (const stat of stats) {
    if (!isDailyStatInRange(stat.date, rangeStartMs)) {
      continue;
    }
    const value = valueOf(stat);
    if (typeof value === "number" && Number.isFinite(value)) {
      total += value;
      seen = true;
    }
  }

  return seen ? total : null;
}

export type PerformanceChartLayer = {
  key: PerformanceChartSeriesKey;
  label: string;
  color: string;
  fill: boolean;
  points: ProfilePerformancePoint[];
};

export type PerformanceChartLayerPath = {
  key: PerformanceChartSeriesKey;
  color: string;
  fill: boolean;
  line: string;
  area?: string;
};

export function buildNormalizedOverlayChartPaths(
  layers: Array<Pick<PerformanceChartLayer, "key" | "color" | "fill" | "points">>,
  options?: { width?: number; height?: number; padTop?: number },
): PerformanceChartLayerPath[] {
  const width = options?.width ?? 640;
  const height = options?.height ?? 260;
  const padX = 18;
  const padTop = options?.padTop ?? 18;
  const padBottom = 30;
  const usableWidth = width - padX * 2;
  const usableHeight = height - padTop - padBottom;

  const drawable = layers
    .map((layer) => ({
      ...layer,
      points: layer.points.filter((point) => Number.isFinite(point.value)),
    }))
    .filter((layer) => layer.points.length >= 2);

  if (drawable.length === 0) {
    return [];
  }

  const allTimes = drawable
    .flatMap((layer) => layer.points.map((point) => Date.parse(point.ts)))
    .filter((value) => Number.isFinite(value));

  if (allTimes.length < 2) {
    return [];
  }

  const minTime = Math.min(...allTimes);
  const maxTime = Math.max(...allTimes);
  const timeSpan = Math.max(maxTime - minTime, 1);

  return drawable.map((layer) => {
    const values = layer.points.map((point) => point.value);
    const minValue = Math.min(...values);
    const maxValue = Math.max(...values);
    const valueSpan = Math.max(maxValue - minValue, 1);

    const coords = layer.points.map((point) => {
      const time = Date.parse(point.ts);
      const x = padX + ((time - minTime) / timeSpan) * usableWidth;
      const y = padTop + usableHeight - ((point.value - minValue) / valueSpan) * usableHeight;
      return [x, y] as const;
    });
    const line = coords
      .map(([x, y], index) => `${index === 0 ? "M" : "L"}${x.toFixed(1)} ${y.toFixed(1)}`)
      .join(" ");
    const first = coords[0];
    const last = coords[coords.length - 1];
    const area = `${line} L${last[0].toFixed(1)} ${height - padBottom} L${first[0].toFixed(1)} ${
      height - padBottom
    } Z`;

    return {
      key: layer.key,
      color: layer.color,
      fill: layer.fill,
      line,
      area: layer.fill ? area : undefined,
    };
  });
}

export function buildSingleChartPath(
  layer: Pick<PerformanceChartLayer, "key" | "color" | "fill" | "points">,
  options?: { width?: number; height?: number },
): PerformanceChartLayerPath | null {
  const width = options?.width ?? 640;
  const height = options?.height ?? 260;
  const padX = 18;
  const padTop = 18;
  const padBottom = 30;
  const usableWidth = width - padX * 2;
  const usableHeight = height - padTop - padBottom;
  const points = layer.points.filter((point) => Number.isFinite(point.value));

  if (points.length < 2) {
    return null;
  }

  const values = points.map((point) => point.value);
  const minValue = Math.min(0, ...values);
  const maxValue = Math.max(0, ...values);
  const valueSpan = Math.max(maxValue - minValue, 1);

  const coords = points.map((point, index) => {
    const x = padX + (usableWidth * index) / Math.max(points.length - 1, 1);
    const y = padTop + usableHeight - ((point.value - minValue) / valueSpan) * usableHeight;
    return [x, y] as const;
  });
  const line = coords
    .map(([x, y], index) => `${index === 0 ? "M" : "L"}${x.toFixed(1)} ${y.toFixed(1)}`)
    .join(" ");
  const first = coords[0];
  const last = coords[coords.length - 1];
  const area = `${line} L${last[0].toFixed(1)} ${height - padBottom} L${first[0].toFixed(1)} ${
    height - padBottom
  } Z`;

  return {
    key: layer.key,
    color: layer.color,
    fill: layer.fill,
    line,
    area: layer.fill ? area : undefined,
  };
}
