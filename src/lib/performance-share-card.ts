import type {
  HomeActivityItem,
  HomePositionItem,
  ProfilePerformancePoint,
} from "./platform-api";

export const PERFORMANCE_SHARE_CARD_WIDTH = 1600;
export const PERFORMANCE_SHARE_CARD_HEIGHT = 900;
export const PERFORMANCE_SHARE_CARD_DOWNLOAD_MIME = "image/jpeg";
export const PERFORMANCE_SHARE_CARD_DOWNLOAD_QUALITY = 0.88;
const PERFORMANCE_SHARE_CARD_ASSET_VERSION = "20260519a";

export const PERFORMANCE_SHARE_CARD_POSITIVE_BACKGROUNDS = [
  "/assets/referral-cards/cat-odds-flip.webp",
  "/assets/referral-cards/squinting-trader.webp",
  "/assets/referral-cards/toast-winner.webp",
  "/assets/referral-cards/absolute-cinema.webp",
  "/assets/referral-cards/cash-cat.webp",
  "/assets/referral-cards/green-market-rainbow.webp",
  "/assets/referral-cards/yellow-suit-entry.webp",
  "/assets/referral-cards/alpha-whisper.webp",
  "/assets/referral-cards/fry-side-eye.webp",
  "/assets/referral-cards/chimp-facepalm.webp",
];

export const PERFORMANCE_SHARE_CARD_NEGATIVE_BACKGROUNDS = [
  "/assets/performance-negative-cards/crying-desk-cat.webp",
  "/assets/performance-negative-cards/jordan-tears.webp",
  "/assets/performance-negative-cards/market-stare.webp",
  "/assets/performance-negative-cards/risk-wojak.webp",
  "/assets/performance-negative-cards/screaming-drawdown.webp",
  "/assets/performance-negative-cards/terminal-wojak.webp",
  "/assets/performance-negative-cards/thumbs-up-cat.webp",
  "/assets/performance-negative-cards/tobey-red-day.webp",
];

export const PERFORMANCE_SHARE_CARD_BACKGROUNDS = PERFORMANCE_SHARE_CARD_POSITIVE_BACKGROUNDS;

type PerformanceCardTheme = "good" | "bad" | "reward" | "neutral";

export type PerformanceShareCardPayload = {
  kind: "daily_pnl" | "position_move" | "liquidity_reward";
  theme: PerformanceCardTheme;
  title: string;
  modeLabel: string;
  mainMetric: string;
  subtitle: string;
  detailPrimary: string;
  detailSecondary: string;
  xText: string;
  filenameSlug: string;
  sparkline: number[];
};

const backgroundDataUrlCache = new Map<string, Promise<string>>();

const moneyFormatter = new Intl.NumberFormat("en-US", {
  style: "currency",
  currency: "USD",
  minimumFractionDigits: 2,
  maximumFractionDigits: 2,
});

const compactNumberFormatter = new Intl.NumberFormat("en-US", {
  maximumFractionDigits: 2,
});

function escapeXml(value: string): string {
  return value
    .replace(/&/g, "&amp;")
    .replace(/</g, "&lt;")
    .replace(/>/g, "&gt;")
    .replace(/"/g, "&quot;")
    .replace(/'/g, "&apos;");
}

function svgToObjectUrl(svg: string): string {
  return URL.createObjectURL(new Blob([svg], { type: "image/svg+xml;charset=utf-8" }));
}

function canvasFont(size: number, weight = 800): string {
  return `${weight} ${size}px system-ui, -apple-system, BlinkMacSystemFont, "Segoe UI", sans-serif`;
}

function roundRectPath(
  context: CanvasRenderingContext2D,
  x: number,
  y: number,
  width: number,
  height: number,
  radius: number,
): void {
  const nextRadius = Math.min(radius, width / 2, height / 2);
  context.beginPath();
  context.moveTo(x + nextRadius, y);
  context.arcTo(x + width, y, x + width, y + height, nextRadius);
  context.arcTo(x + width, y + height, x, y + height, nextRadius);
  context.arcTo(x, y + height, x, y, nextRadius);
  context.arcTo(x, y, x + width, y, nextRadius);
  context.closePath();
}

function fillRoundRect(
  context: CanvasRenderingContext2D,
  x: number,
  y: number,
  width: number,
  height: number,
  radius: number,
  fillStyle: string | CanvasGradient | CanvasPattern,
): void {
  roundRectPath(context, x, y, width, height, radius);
  context.fillStyle = fillStyle;
  context.fill();
}

function strokeRoundRect(
  context: CanvasRenderingContext2D,
  x: number,
  y: number,
  width: number,
  height: number,
  radius: number,
  strokeStyle: string | CanvasGradient | CanvasPattern,
  lineWidth = 1,
): void {
  roundRectPath(context, x, y, width, height, radius);
  context.strokeStyle = strokeStyle;
  context.lineWidth = lineWidth;
  context.stroke();
}

function fillText(
  context: CanvasRenderingContext2D,
  value: string,
  x: number,
  y: number,
  options: {
    size: number;
    weight?: number;
    color: string;
    align?: CanvasTextAlign;
    shadowColor?: string;
    shadowBlur?: number;
  },
): void {
  context.save();
  context.font = canvasFont(options.size, options.weight ?? 800);
  context.fillStyle = options.color;
  context.textAlign = options.align ?? "left";
  context.textBaseline = "alphabetic";
  if (options.shadowColor || options.shadowBlur) {
    context.shadowColor = options.shadowColor ?? options.color;
    context.shadowBlur = options.shadowBlur ?? 0;
  }
  context.fillText(value, x, y);
  context.restore();
}

function formatMoney(value: number | null | undefined, options?: { signed?: boolean; absolute?: boolean }): string {
  if (typeof value !== "number" || !Number.isFinite(value)) {
    return "--";
  }
  const displayValue = options?.absolute ? Math.abs(value) : value;
  const formatted = moneyFormatter.format(Math.abs(displayValue));
  if (!options?.signed) {
    return displayValue < 0 ? `-${formatted}` : formatted;
  }
  if (displayValue > 0) {
    return `+${formatted}`;
  }
  if (displayValue < 0) {
    return `-${formatted}`;
  }
  return formatted;
}

function formatControlValue(value: number | null | undefined): string {
  if (typeof value !== "number" || !Number.isFinite(value)) {
    return "--";
  }
  return compactNumberFormatter.format(value);
}

function formatPriceCents(value: number | null | undefined): string {
  if (typeof value !== "number" || !Number.isFinite(value)) {
    return "--";
  }
  const cents = value * 100;
  const digits = Math.abs(cents % 1) > 0 ? 1 : 0;
  return `${cents.toFixed(digits)}c`;
}

function formatPercent(value: number | null | undefined): string {
  if (typeof value !== "number" || !Number.isFinite(value)) {
    return "--";
  }
  return `${value >= 0 ? "+" : ""}${value.toFixed(2)}%`;
}

function shortenUrl(url: string): string {
  return url.replace(/^https?:\/\//, "");
}

function slugify(value: string): string {
  return value
    .toLowerCase()
    .replace(/[^a-z0-9]+/g, "-")
    .replace(/^-+|-+$/g, "")
    .slice(0, 48) || "card";
}

function themeAccent(theme: PerformanceCardTheme): { primary: string; secondary: string; mode: string } {
  if (theme === "bad") {
    return { primary: "#ff5f76", secondary: "#ffad4d", mode: "RED" };
  }
  if (theme === "reward") {
    return { primary: "#58f29a", secondary: "#ffd166", mode: "PAID" };
  }
  if (theme === "neutral") {
    return { primary: "#67c3ff", secondary: "#9fb2c8", mode: "FLAT" };
  }
  return { primary: "#47f279", secondary: "#55a8ff", mode: "GREEN" };
}

function pnlTheme(value: number | null | undefined): PerformanceCardTheme {
  if (typeof value !== "number" || !Number.isFinite(value) || value === 0) {
    return "neutral";
  }
  return value > 0 ? "good" : "bad";
}

function normalizeSparkline(values: number[]): number[] {
  const finite = values.filter((value) => Number.isFinite(value));
  if (finite.length >= 2) {
    return finite;
  }
  return [0, 0, 0, 0, 0, 0, 0, 0];
}

function positionSparkline(position: HomePositionItem): number[] {
  const avg = typeof position.avg_price === "number" ? position.avg_price : null;
  const current = typeof position.current_price === "number" ? position.current_price : null;
  if (avg === null || current === null) {
    return [0, 0, 0, 0, 0, 0];
  }
  return [avg, avg * 0.99, avg * 1.01, (avg + current) / 2, current * 1.01, current];
}

function getPositionTraded(position: HomePositionItem): number | null {
  if (typeof position.initial_value === "number") return position.initial_value;
  if (typeof position.total_bought === "number") return position.total_bought;
  if (
    typeof position.size === "number" &&
    typeof position.avg_price === "number" &&
    Number.isFinite(position.size) &&
    Number.isFinite(position.avg_price)
  ) {
    return position.size * position.avg_price;
  }
  return null;
}

function getPositionPnl(position: HomePositionItem): number | null {
  if (typeof position.cash_pnl === "number") return position.cash_pnl;
  const value = position.current_value;
  const traded = getPositionTraded(position);
  if (typeof value === "number" && typeof traded === "number") {
    return value - traded;
  }
  return null;
}

function getPositionPercentPnl(position: HomePositionItem): number | null {
  if (typeof position.percent_pnl === "number") return position.percent_pnl;
  const pnl = getPositionPnl(position);
  const traded = getPositionTraded(position);
  if (typeof pnl === "number" && typeof traded === "number" && traded !== 0) {
    return (pnl / traded) * 100;
  }
  return null;
}

export function pickPerformanceShareCardBackground(card: PerformanceShareCardPayload): string {
  const pool =
    card.kind === "liquidity_reward" || card.theme === "good" || card.theme === "reward"
      ? PERFORMANCE_SHARE_CARD_POSITIVE_BACKGROUNDS
      : PERFORMANCE_SHARE_CARD_NEGATIVE_BACKGROUNDS;
  return (
    pool[
      Math.floor(Math.random() * pool.length)
    ] ?? pool[0] ?? PERFORMANCE_SHARE_CARD_POSITIVE_BACKGROUNDS[0]
  );
}

export function performanceShareCardBackgroundUrl(backgroundPath: string): string {
  return `${backgroundPath}?v=${PERFORMANCE_SHARE_CARD_ASSET_VERSION}`;
}

async function fetchAsDataUrl(url: string): Promise<string> {
  const response = await fetch(url);
  if (!response.ok) {
    throw new Error("performance_share_card_asset_unavailable");
  }
  const blob = await response.blob();
  return await new Promise<string>((resolve, reject) => {
    const reader = new FileReader();
    reader.onloadend = () => resolve(String(reader.result));
    reader.onerror = () => reject(new Error("performance_share_card_asset_read_failed"));
    reader.readAsDataURL(blob);
  });
}

async function loadPerformanceShareCardBackground(backgroundPath: string): Promise<string> {
  const url = performanceShareCardBackgroundUrl(backgroundPath);
  const cached = backgroundDataUrlCache.get(url);
  if (cached) {
    return cached;
  }
  const pending = fetchAsDataUrl(url).catch((error) => {
    backgroundDataUrlCache.delete(url);
    throw error;
  });
  backgroundDataUrlCache.set(url, pending);
  return pending;
}

async function loadImage(src: string, errorMessage: string): Promise<HTMLImageElement> {
  const image = new Image();
  image.decoding = "async";
  await new Promise<void>((resolve, reject) => {
    image.onload = () => resolve();
    image.onerror = () => reject(new Error(errorMessage));
    image.src = src;
  });
  return image;
}

function wrapWords(value: string, maxChars: number, maxLines: number): string[] {
  const words = value.trim().split(/\s+/).filter(Boolean);
  const lines: string[] = [];
  let current = "";
  for (const word of words) {
    const next = current ? `${current} ${word}` : word;
    if (next.length <= maxChars || !current) {
      current = next;
      continue;
    }
    lines.push(current);
    current = word;
    if (lines.length >= maxLines) {
      break;
    }
  }
  if (current && lines.length < maxLines) {
    lines.push(current);
  }
  if (lines.length === maxLines && words.join(" ").length > lines.join(" ").length) {
    lines[maxLines - 1] = `${lines[maxLines - 1].replace(/\s+\S*$/, "")}...`;
  }
  return lines.length > 0 ? lines : [value];
}

function textLinesSvg(lines: string[], x: number, y: number, fontSize: number, lineHeight: number, weight = 800): string {
  return lines
    .map(
      (line, index) => `
        <text x="${x}" y="${y + index * lineHeight}" fill="#e6f0f8"
          font-size="${fontSize}" font-weight="${weight}" font-family="system-ui, -apple-system, BlinkMacSystemFont, 'Segoe UI', sans-serif">
          ${escapeXml(line)}
        </text>`,
    )
    .join("");
}

function sparklineSvg(values: number[], color: string): string {
  const normalized = normalizeSparkline(values);
  const min = Math.min(...normalized);
  const max = Math.max(...normalized);
  const span = Math.max(max - min, 0.000001);
  const x = 178;
  const y = 548;
  const width = 480;
  const height = 54;
  const points = normalized.map((value, index) => {
    const px = x + (index / Math.max(normalized.length - 1, 1)) * width;
    const py = y + height - ((value - min) / span) * height;
    return [Number(px.toFixed(1)), Number(py.toFixed(1))] as const;
  });
  const path = points.map(([px, py], index) => `${index === 0 ? "M" : "L"}${px},${py}`).join(" ");
  const area = `${path} L${x + width},${y + height + 10} L${x},${y + height + 10} Z`;
  return `
    <rect x="160" y="526" width="516" height="96" rx="15" fill="#030d10" stroke="#ffffff" stroke-opacity="0.1" stroke-width="1"/>
    <path d="${area}" fill="${color}" opacity="0.22"/>
    <path d="${path}" fill="none" stroke="${color}" stroke-width="7" stroke-linecap="round" stroke-linejoin="round"/>
  `;
}

function drawSparklineCanvas(context: CanvasRenderingContext2D, values: number[], color: string): void {
  const normalized = normalizeSparkline(values);
  const min = Math.min(...normalized);
  const max = Math.max(...normalized);
  const span = Math.max(max - min, 0.000001);
  const x = 178;
  const y = 548;
  const width = 480;
  const height = 54;
  const points = normalized.map((value, index) => {
    const px = x + (index / Math.max(normalized.length - 1, 1)) * width;
    const py = y + height - ((value - min) / span) * height;
    return [px, py] as const;
  });

  context.save();
  fillRoundRect(context, 160, 526, 516, 96, 15, "#030d10");
  context.globalAlpha = 0.1;
  strokeRoundRect(context, 160, 526, 516, 96, 15, "#ffffff", 1);
  context.globalAlpha = 1;

  context.beginPath();
  points.forEach(([px, py], index) => {
    if (index === 0) {
      context.moveTo(px, py);
    } else {
      context.lineTo(px, py);
    }
  });
  context.lineTo(x + width, y + height + 10);
  context.lineTo(x, y + height + 10);
  context.closePath();
  context.fillStyle = color;
  context.globalAlpha = 0.22;
  context.fill();

  context.beginPath();
  points.forEach(([px, py], index) => {
    if (index === 0) {
      context.moveTo(px, py);
    } else {
      context.lineTo(px, py);
    }
  });
  context.globalAlpha = 1;
  context.strokeStyle = color;
  context.lineWidth = 7;
  context.lineCap = "round";
  context.lineJoin = "round";
  context.stroke();
  context.restore();
}

function drawPerformanceShareCardCanvas(
  context: CanvasRenderingContext2D,
  card: PerformanceShareCardPayload,
  shareUrl?: string | null,
): void {
  const { primary, secondary } = themeAccent(card.theme);
  const shortLink = shareUrl ? shortenUrl(shareUrl) : null;
  const mainFontSize = card.mainMetric.length > 9 ? 70 : 82;
  const linkFontSize = shortLink
    ? shortLink.length > 38
      ? 22
      : shortLink.length > 32
        ? 24
        : 26
    : 26;
  const detailPrimaryFontSize = card.detailPrimary.length > 44 ? 17 : 18;
  const detailSecondaryFontSize = card.detailSecondary.length > 44 ? 17 : 18;
  const subtitleLines = wrapWords(card.subtitle, 46, 2);

  context.save();
  context.fillStyle = "rgba(0, 0, 0, 0.08)";
  context.fillRect(0, 0, PERFORMANCE_SHARE_CARD_WIDTH, PERFORMANCE_SHARE_CARD_HEIGHT);

  const seam = context.createLinearGradient(688, 0, 898, 0);
  seam.addColorStop(0, "rgba(2, 7, 10, 0.98)");
  seam.addColorStop(0.56, "rgba(2, 7, 10, 0.9)");
  seam.addColorStop(1, "rgba(2, 7, 10, 0)");
  context.fillStyle = seam;
  context.fillRect(688, 0, 210, 900);

  context.save();
  context.globalAlpha = 0.16;
  context.strokeStyle = primary;
  context.lineWidth = 1;
  for (let offsetX = 688; offsetX <= 898; offsetX += 68) {
    context.beginPath();
    context.moveTo(offsetX, 0);
    context.lineTo(offsetX, 900);
    context.stroke();
  }
  for (let offsetY = 0; offsetY <= 900; offsetY += 68) {
    context.beginPath();
    context.moveTo(688, offsetY);
    context.lineTo(898, offsetY);
    context.stroke();
  }
  context.restore();

  const panelFill = context.createLinearGradient(92, 76, 716, 820);
  panelFill.addColorStop(0, "rgba(2, 7, 6, 0.96)");
  panelFill.addColorStop(1, "rgba(6, 20, 16, 0.92)");
  fillRoundRect(context, 92, 76, 624, 744, 32, panelFill);
  strokeRoundRect(context, 92, 76, 624, 744, 32, primary, 3);

  fillRoundRect(context, 160, 116, 116, 100, 16, "#020e0a");
  strokeRoundRect(context, 160, 116, 116, 100, 16, primary, 2);
  fillText(context, "ev", 184, 184, { size: 31, weight: 900, color: "#ffffff" });
  fillText(context, "+", 225, 184, { size: 31, weight: 900, color: primary });

  fillText(context, "EVPLUS", 320, 180, { size: 53, weight: 900, color: "#f6fff8" });
  fillText(context, "PREDICT. TRADE. PROFIT.", 324, 216, { size: 19, weight: 800, color: "#dbe7f5" });

  context.beginPath();
  context.moveTo(160, 250);
  context.lineTo(676, 250);
  context.strokeStyle = primary;
  context.lineWidth = 2;
  context.globalAlpha = 0.78;
  context.stroke();
  context.globalAlpha = 1;

  fillText(context, card.title, 160, 322, { size: 31, weight: 900, color: "#ffffff" });
  fillRoundRect(context, 560, 282, 116, 48, 13, "rgba(0, 0, 0, 0.42)");
  strokeRoundRect(context, 560, 282, 116, 48, 13, primary, 2);
  fillText(context, card.modeLabel, 618, 313, { size: 17, weight: 900, color: primary, align: "center" });

  fillText(context, card.mainMetric, 160, 430, {
    size: mainFontSize,
    weight: 900,
    color: primary,
    shadowColor: primary,
    shadowBlur: 16,
  });

  subtitleLines.forEach((line, index) => {
    fillText(context, line, 160, 468 + index * 28, { size: 22, weight: 800, color: "#e6f0f8" });
  });

  drawSparklineCanvas(context, card.sparkline, secondary);

  fillText(context, card.detailPrimary, 160, 666, {
    size: detailPrimaryFontSize,
    weight: 800,
    color: "#f4faff",
  });
  fillText(context, card.detailSecondary, 160, 692, {
    size: detailSecondaryFontSize,
    weight: 500,
    color: "#aabdd1",
  });

  if (shortLink) {
    fillRoundRect(context, 160, 738, 516, 58, 15, "rgba(0, 0, 0, 0.45)");
    strokeRoundRect(context, 160, 738, 516, 58, 15, primary, 2);
    fillText(context, shortLink, 184, 775, { size: linkFontSize, weight: 900, color: "#ffffff" });
  }
  context.restore();
}

export function buildPerformanceShareCardSvg(card: PerformanceShareCardPayload, shareUrl?: string | null): string {
  const { primary, secondary } = themeAccent(card.theme);
  const shortLink = shareUrl ? shortenUrl(shareUrl) : null;
  const mainFontSize = card.mainMetric.length > 9 ? 70 : 82;
  const linkFontSize = shortLink
    ? shortLink.length > 38
      ? 22
      : shortLink.length > 32
        ? 24
        : 26
    : 26;
  const detailPrimaryFontSize = card.detailPrimary.length > 44 ? 17 : 18;
  const detailSecondaryFontSize = card.detailSecondary.length > 44 ? 17 : 18;
  const subtitleLines = wrapWords(card.subtitle, 46, 2);

  return `
    <svg width="${PERFORMANCE_SHARE_CARD_WIDTH}" height="${PERFORMANCE_SHARE_CARD_HEIGHT}"
      viewBox="0 0 ${PERFORMANCE_SHARE_CARD_WIDTH} ${PERFORMANCE_SHARE_CARD_HEIGHT}" xmlns="http://www.w3.org/2000/svg">
      <defs>
        <linearGradient id="performancePanel" x1="0" x2="1" y1="0" y2="1">
          <stop offset="0%" stop-color="#020706" stop-opacity="0.96"/>
          <stop offset="100%" stop-color="#061410" stop-opacity="0.92"/>
        </linearGradient>
        <linearGradient id="oldPanelSeamMask" x1="0" x2="1" y1="0" y2="0">
          <stop offset="0%" stop-color="#02070a" stop-opacity="0.98"/>
          <stop offset="56%" stop-color="#02070a" stop-opacity="0.9"/>
          <stop offset="100%" stop-color="#02070a" stop-opacity="0"/>
        </linearGradient>
        <pattern id="cleanupGrid" width="68" height="68" patternUnits="userSpaceOnUse">
          <path d="M68 0H0V68" fill="none" stroke="${primary}" stroke-width="1" opacity="0.18"/>
        </pattern>
        <filter id="mainGlow" x="-20%" y="-35%" width="140%" height="170%">
          <feGaussianBlur stdDeviation="8" result="blur"/>
          <feMerge>
            <feMergeNode in="blur"/>
            <feMergeNode in="SourceGraphic"/>
          </feMerge>
        </filter>
      </defs>
      <rect width="1600" height="900" fill="#000000" opacity="0.08"/>
      <rect x="688" y="0" width="210" height="900" fill="url(#oldPanelSeamMask)"/>
      <rect x="688" y="0" width="210" height="900" fill="url(#cleanupGrid)" opacity="0.16"/>
      <rect x="92" y="76" width="624" height="744" rx="32" fill="url(#performancePanel)" stroke="${primary}" stroke-width="3"/>
      <rect x="160" y="116" width="116" height="100" rx="16" fill="#020e0a" stroke="${primary}" stroke-width="2"/>
      <text x="184" y="184" fill="#ffffff" font-size="31" font-weight="900"
        font-family="system-ui, -apple-system, BlinkMacSystemFont, 'Segoe UI', sans-serif">
        ev<tspan fill="${primary}">+</tspan>
      </text>
      <text x="320" y="180" fill="#f6fff8" font-size="53" font-weight="900" letter-spacing="5"
        font-family="system-ui, -apple-system, BlinkMacSystemFont, 'Segoe UI', sans-serif">
        EVPLUS
      </text>
      <text x="324" y="216" fill="#dbe7f5" font-size="19" font-weight="800"
        font-family="system-ui, -apple-system, BlinkMacSystemFont, 'Segoe UI', sans-serif">
        PREDICT. TRADE. PROFIT.
      </text>
      <line x1="160" y1="250" x2="676" y2="250" stroke="${primary}" stroke-width="2" opacity="0.78"/>
      <text x="160" y="322" fill="#ffffff" font-size="31" font-weight="900"
        font-family="system-ui, -apple-system, BlinkMacSystemFont, 'Segoe UI', sans-serif">
        ${escapeXml(card.title)}
      </text>
      <rect x="560" y="282" width="116" height="48" rx="13" fill="#000000" opacity="0.42" stroke="${primary}" stroke-width="2"/>
      <text x="618" y="313" text-anchor="middle" fill="${primary}" font-size="17" font-weight="900"
        font-family="system-ui, -apple-system, BlinkMacSystemFont, 'Segoe UI', sans-serif">
        ${escapeXml(card.modeLabel)}
      </text>
      <text x="160" y="430" fill="${primary}" font-size="${mainFontSize}" font-weight="900" filter="url(#mainGlow)"
        font-family="system-ui, -apple-system, BlinkMacSystemFont, 'Segoe UI', sans-serif">
        ${escapeXml(card.mainMetric)}
      </text>
      ${textLinesSvg(subtitleLines, 160, 468, 22, 28, 800)}
      ${sparklineSvg(card.sparkline, secondary)}
      <text x="160" y="666" fill="#f4faff" font-size="${detailPrimaryFontSize}" font-weight="800"
        font-family="system-ui, -apple-system, BlinkMacSystemFont, 'Segoe UI', sans-serif">
        ${escapeXml(card.detailPrimary)}
      </text>
      <text x="160" y="692" fill="#aabdd1" font-size="${detailSecondaryFontSize}" font-weight="500"
        font-family="system-ui, -apple-system, BlinkMacSystemFont, 'Segoe UI', sans-serif">
        ${escapeXml(card.detailSecondary)}
      </text>
      ${
        shortLink
          ? `<rect x="160" y="738" width="516" height="58" rx="15" fill="#000000" opacity="0.45" stroke="${primary}" stroke-width="2"/>
      <text x="184" y="775" fill="#ffffff" font-size="${linkFontSize}" font-weight="900"
        font-family="system-ui, -apple-system, BlinkMacSystemFont, 'Segoe UI', sans-serif">
        ${escapeXml(shortLink)}
      </text>`
          : ""
      }
    </svg>
  `;
}

export function createPerformanceShareOverlayUrl(card: PerformanceShareCardPayload, shareUrl?: string | null): string {
  return svgToObjectUrl(buildPerformanceShareCardSvg(card, shareUrl));
}

export async function renderPerformanceShareCardBlob(
  card: PerformanceShareCardPayload,
  backgroundPath: string,
  shareUrl?: string | null,
  options?: { mimeType?: string; quality?: number },
): Promise<Blob> {
  const background = await loadPerformanceShareCardBackground(backgroundPath);
  const backgroundImage = await loadImage(background, "performance_share_card_background_failed");
  const canvas = document.createElement("canvas");
  canvas.width = PERFORMANCE_SHARE_CARD_WIDTH;
  canvas.height = PERFORMANCE_SHARE_CARD_HEIGHT;
  const context = canvas.getContext("2d");
  if (!context) {
    throw new Error("performance_share_card_canvas_unavailable");
  }
  context.drawImage(backgroundImage, 0, 0, PERFORMANCE_SHARE_CARD_WIDTH, PERFORMANCE_SHARE_CARD_HEIGHT);
  drawPerformanceShareCardCanvas(context, card, shareUrl);
  const blob = await new Promise<Blob | null>((resolve) =>
    canvas.toBlob(resolve, options?.mimeType ?? "image/png", options?.quality),
  );
  if (!blob) {
    throw new Error("performance_share_card_blob_failed");
  }
  return blob;
}

export function buildPerformanceShareOnXUrl(card: PerformanceShareCardPayload, shareUrl?: string | null): string {
  const params = new URLSearchParams({
    text: `${card.title}: ${card.mainMetric} on EVPLUS. ${card.xText}`,
  });
  if (shareUrl) {
    params.set("url", shareUrl);
  }
  return `https://twitter.com/intent/tweet?${params.toString()}`;
}

export function buildDailyPnlShareCard(input: {
  pnl: number | null;
  openPnl: number | null;
  realizedPnl: number | null;
  sourceLabel: string;
  feedLabel: string;
  updatedLabel: string | null;
  series: ProfilePerformancePoint[];
}): PerformanceShareCardPayload | null {
  if (typeof input.pnl !== "number" || !Number.isFinite(input.pnl)) {
    return null;
  }
  const theme = pnlTheme(input.pnl);
  const { mode } = themeAccent(theme);
  const metric = formatMoney(input.pnl, { signed: true });
  const openLabel = input.openPnl === null ? "Open pending" : `Open ${formatMoney(input.openPnl)}`;
  const realizedLabel = input.realizedPnl === null ? "Realized pending" : `Realized ${formatMoney(input.realizedPnl)}`;
  const feedLabel = input.feedLabel.trim() || "synced";
  return {
    kind: "daily_pnl",
    theme,
    title: "24H PNL",
    modeLabel: mode,
    mainMetric: metric,
    subtitle: "Polymarket account movement for the past day.",
    detailPrimary: `${openLabel} | ${realizedLabel}`,
    detailSecondary: `${input.sourceLabel} feed ${feedLabel.toLowerCase()} | ${
      input.updatedLabel ? `Updated ${input.updatedLabel}` : "Updated pending"
    }`,
    xText: "Track PnL and trade predictions with me.",
    filenameSlug: `24h-pnl-${theme}`,
    sparkline: normalizeSparkline(input.series.map((point) => point.value)),
  };
}

export function buildPositionMoveShareCard(position: HomePositionItem): PerformanceShareCardPayload | null {
  const pnl = getPositionPnl(position);
  if (typeof pnl !== "number" || !Number.isFinite(pnl)) {
    return null;
  }
  const pnlPercent = getPositionPercentPnl(position);
  const theme = pnlTheme(pnl);
  const { mode } = themeAccent(theme);
  const title = position.market_title?.trim() || "Open position";
  const avgPrice = formatPriceCents(position.avg_price);
  const currentPrice = formatPriceCents(position.current_price);
  const metric = formatMoney(pnl, { signed: true });
  const pctLabel = pnlPercent === null ? "--" : formatPercent(pnlPercent);
  return {
    kind: "position_move",
    theme,
    title: "POSITION PNL",
    modeLabel: mode,
    mainMetric: metric,
    subtitle: title,
    detailPrimary: `${position.outcome ?? "Position"} ${avgPrice} -> ${currentPrice} | ${formatControlValue(position.size)} shares`,
    detailSecondary: `Position ${pnl >= 0 ? "up" : "down"} ${pctLabel}`,
    xText: "See the market move with me.",
    filenameSlug: `position-pnl-${slugify(title)}`,
    sparkline: positionSparkline(position),
  };
}

export function buildLiquidityRewardShareCard(input: {
  reward: number | null;
  title?: string | null;
  shares?: number | null;
  detail?: string | null;
  updatedLabel?: string | null;
}): PerformanceShareCardPayload | null {
  if (typeof input.reward !== "number" || !Number.isFinite(input.reward) || input.reward <= 0) {
    return null;
  }
  const title = input.title?.trim() || "Liquidity rewards";
  const metric = formatMoney(input.reward, { signed: true, absolute: true });
  const sharesLabel = typeof input.shares === "number" && Number.isFinite(input.shares)
    ? `${formatControlValue(input.shares)} shares`
    : "Maker reward";
  const cleanDetail = input.detail?.trim().replace(/\s*\|\s*Updated\s+.+$/i, "") ?? "";
  const updatedDetail = input.updatedLabel
    ? input.updatedLabel.toLowerCase().startsWith("updated")
      ? input.updatedLabel
      : `Updated ${input.updatedLabel}`
    : "Updated recently";
  return {
    kind: "liquidity_reward",
    theme: "reward",
    title: "LIQUIDITY REWARD",
    modeLabel: "PAID",
    mainMetric: metric,
    subtitle: title,
    detailPrimary: cleanDetail || `Maker reward credited | ${sharesLabel}`,
    detailSecondary: `Reward paid | ${updatedDetail}`,
    xText: "Reward paid on EVPLUS.",
    filenameSlug: `liquidity-reward-${slugify(title)}`,
    sparkline: [0, 0.02, 0.05, 0.12, 0.1, 0.22, 0.28, 0.36, 0.34, 0.48, 0.52, 0.62],
  };
}

export function buildRewardActivityShareCard(item: HomeActivityItem): PerformanceShareCardPayload | null {
  const reward = Math.abs(item.cashflow_usd ?? item.value_usd ?? 0);
  if (!Number.isFinite(reward) || reward <= 0) {
    return null;
  }
  return buildLiquidityRewardShareCard({
    reward,
    title: item.market_title || item.title || "Rewards distributed for epoch",
    shares: item.quantity ?? null,
    detail: item.detail,
    updatedLabel: "Activity feed",
  });
}
