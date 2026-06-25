import { useEffect, useMemo, useRef, useState } from "react";
import { SectionPanel } from "./SectionPanel";
import type {
  HomeActivityItem,
  HomeOpenOrderItem,
  HomePositionItem,
  ProfilePerformanceRange,
} from "../lib/platform-api";
import type { HomePerformanceSnapshot } from "../lib/home-performance-snapshot";
import {
  buildSingleChartPath,
  getPerformanceRangeStartMs,
  PERFORMANCE_CHART_SERIES,
  sumDailyStatInRange,
  type PerformanceChartSeriesKey,
} from "../lib/home-performance-chart-series";
import {
  buildPositionMoveShareCard,
  buildRewardActivityShareCard,
  type PerformanceShareCardPayload,
} from "../lib/performance-share-card";
import { buildPolymarketMarketUrl } from "../lib/polymarket-links";
import {
  getHomeActivityApi,
  getHomeOpenOrdersApi,
  getHomePositionsApi,
} from "../lib/tauri-commands";

function formatRelativeTime(value: string | null | undefined): string {
  if (!value) return "--";
  const date = new Date(value);
  if (Number.isNaN(date.getTime())) return value;
  const diffSeconds = Math.round((date.getTime() - Date.now()) / 1000);
  const abs = Math.abs(diffSeconds);
  const rtf = new Intl.RelativeTimeFormat("en", { numeric: "auto" });

  if (abs < 60) return rtf.format(diffSeconds, "second");
  if (abs < 3600) return rtf.format(Math.round(diffSeconds / 60), "minute");
  if (abs < 86400) return rtf.format(Math.round(diffSeconds / 3600), "hour");
  return rtf.format(Math.round(diffSeconds / 86400), "day");
}

function isAbortError(error: unknown): boolean {
  return error instanceof DOMException && error.name === "AbortError";
}

function isDocumentVisible(): boolean {
  return typeof document === "undefined" || document.visibilityState === "visible";
}

type HomePortfolioTab = "activity" | "positions" | "open-orders" | "performance";

const INITIAL_ACTIVITY_LOAD_DELAY_MS = 250;
const TAB_REFRESH_INTERVAL_MS: Record<Exclude<HomePortfolioTab, "performance">, number> = {
  activity: 15_000,
  positions: 30_000,
  "open-orders": 20_000,
};

const PERFORMANCE_RANGE_OPTIONS: Array<{ range: ProfilePerformanceRange; label: string }> = [
  { range: "6h", label: "6H" },
  { range: "1d", label: "1D" },
  { range: "7d", label: "7D" },
  { range: "30d", label: "30D" },
  { range: "all", label: "ALL" },
];

type HomeTabState<T> = {
  items: T[];
  error: string | null;
  isLoading: boolean;
  loaded: boolean;
};

const EMPTY_TAB_STATE = {
  items: [],
  error: null,
  isLoading: false,
  loaded: false,
};

function formatMoney(value: number | null | undefined): string {
  if (typeof value !== "number" || !Number.isFinite(value)) {
    return "--";
  }
  return new Intl.NumberFormat("en-US", {
    style: "currency",
    currency: "USD",
    minimumFractionDigits: 2,
    maximumFractionDigits: 2,
  }).format(value);
}

function formatControlValue(value: number): string {
  return new Intl.NumberFormat("en-US", {
    maximumFractionDigits: value % 1 === 0 ? 0 : 2,
  }).format(value);
}

function formatPriceCents(value: number | null | undefined): string {
  if (typeof value !== "number" || !Number.isFinite(value)) {
    return "--";
  }
  const cents = value * 100;
  const digits = Math.abs(cents % 1) > 0 ? 1 : 0;
  return `${cents.toFixed(digits)}c`;
}

function formatPercent(value: number | null | undefined): string | null {
  if (typeof value !== "number" || !Number.isFinite(value)) {
    return null;
  }
  return `${value >= 0 ? "+" : ""}${value.toFixed(2)}%`;
}

function formatSignedMoney(value: number | null | undefined): string {
  if (typeof value !== "number" || !Number.isFinite(value)) {
    return "--";
  }
  const formatted = formatMoney(Math.abs(value));
  return value > 0 ? `+${formatted}` : value < 0 ? `-${formatted}` : formatted;
}

function formatCount(value: number | null | undefined): string {
  if (typeof value !== "number" || !Number.isFinite(value)) {
    return "--";
  }
  return new Intl.NumberFormat("en-US", {
    maximumFractionDigits: 0,
  }).format(value);
}

function valueToneClass(value: number | null | undefined): "positive" | "negative" | "neutral" {
  if (typeof value !== "number" || !Number.isFinite(value) || value === 0) return "neutral";
  return value > 0 ? "positive" : "negative";
}

function readErrorMessage(error: unknown): string {
  if (error instanceof Error && error.message.trim()) {
    return error.message.trim();
  }
  if (typeof error === "object" && error !== null) {
    if ("message" in error && typeof error.message === "string" && error.message.trim()) {
      return error.message.trim();
    }
    if ("error" in error && typeof error.error === "string" && error.error.trim()) {
      return error.error.trim();
    }
  }
  return String(error);
}

function isTransientWorkerRecoveryError(error: string | null | undefined): boolean {
  const normalized = String(error ?? "").trim().toLowerCase();
  return (
    normalized === "worker_unreachable" ||
    normalized === "worker_missing" ||
    normalized === "runtime_admin_unavailable"
  );
}

function isRewardActivity(item: HomeActivityItem): boolean {
  const action = item.action?.trim().toLowerCase() ?? "";
  const kind = item.kind?.trim().toLowerCase() ?? "";
  return item.is_reward === true || action === "reward" || kind === "maker_rebate";
}

function activityActionClass(item: HomeActivityItem): "buy" | "sell" | "reward" {
  if (isRewardActivity(item)) {
    return "reward";
  }
  return item.action?.toLowerCase() === "sold" ? "sell" : "buy";
}

function activityOutcomeClass(outcome?: string | null): "positive" | "negative" | "neutral" {
  const lower = outcome?.toLowerCase() ?? "";
  if (lower.startsWith("yes") || lower.startsWith("up")) return "positive";
  if (lower.startsWith("no") || lower.startsWith("down")) return "negative";
  return "neutral";
}

function activityValueClass(value?: number | null): "positive" | "negative" | "neutral" {
  if (typeof value !== "number" || !Number.isFinite(value) || value === 0) return "neutral";
  return value > 0 ? "positive" : "negative";
}

function shouldShowActivityPrice(item: HomeActivityItem): boolean {
  if (isRewardActivity(item)) {
    return false;
  }
  if (typeof item.price !== "number" || !Number.isFinite(item.price) || item.price <= 0) {
    return false;
  }
  const action = item.action?.trim().toLowerCase() ?? "";
  return action === "bought" || action === "sold";
}

function outcomeTone(outcome?: string | null): "positive" | "negative" | "neutral" {
  return activityOutcomeClass(outcome);
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

type OpenOrderGroup = {
  key: string;
  marketTitle: string;
  marketSlug: string | null;
  eventSlug: string | null;
  thumbnailUrl: string | null;
  orders: HomeOpenOrderItem[];
  totalOriginal: number;
  totalMatched: number;
  totalRemaining: number;
  totalNotionalUsd: number;
  nearestExpiration: string | null;
};

function groupOpenOrders(rows: HomeOpenOrderItem[]): OpenOrderGroup[] {
  const groups = new Map<string, OpenOrderGroup>();

  rows.forEach((row) => {
    const key = row.condition_id || row.market_title || row.id;
    const current = groups.get(key);
    const original = row.original_size ?? 0;
    const matched = row.size_matched ?? 0;
    const remaining = row.remaining_size ?? 0;
    const total = row.total_notional_usd ?? 0;

    if (current) {
      current.orders.push(row);
      current.marketSlug = current.marketSlug ?? row.market_slug ?? null;
      current.eventSlug = current.eventSlug ?? row.event_slug ?? null;
      current.totalOriginal += original;
      current.totalMatched += matched;
      current.totalRemaining += remaining;
      current.totalNotionalUsd += total;
      if (
        row.expiration &&
        (!current.nearestExpiration ||
          new Date(row.expiration).getTime() < new Date(current.nearestExpiration).getTime())
      ) {
        current.nearestExpiration = row.expiration;
      }
      return;
    }

    groups.set(key, {
      key,
      marketTitle: row.market_title || "Unknown market",
      marketSlug: row.market_slug ?? null,
      eventSlug: row.event_slug ?? null,
      thumbnailUrl: row.thumbnail_url ?? null,
      orders: [row],
      totalOriginal: original,
      totalMatched: matched,
      totalRemaining: remaining,
      totalNotionalUsd: total,
      nearestExpiration: row.expiration ?? null,
    });
  });

  return Array.from(groups.values()).sort((left, right) => right.totalNotionalUsd - left.totalNotionalUsd);
}

function SearchIcon() {
  return (
    <svg aria-hidden="true" viewBox="0 0 24 24" className="home-portfolio-tabs__search-icon">
      <path
        d="M10.5 4.5a6 6 0 1 0 3.79 10.65l4.28 4.27 1.42-1.41-4.27-4.28A6 6 0 0 0 10.5 4.5Zm0 2a4 4 0 1 1 0 8 4 4 0 0 1 0-8Z"
        fill="currentColor"
      />
    </svg>
  );
}

function RewardIcon() {
  return (
    <svg aria-hidden="true" viewBox="0 0 24 24" className="activity-feed__reward-icon">
      <path
        d="M6 8.5A2.5 2.5 0 1 1 8.5 6H12a2.5 2.5 0 1 1 4.5 2.5H18a1 1 0 0 1 1 1V12h-6v8h-2v-8H5V9.5a1 1 0 0 1 1-1h0Zm.5-1h2a1 1 0 1 0-1-1 1 1 0 0 0-1 1Zm8.5 0h2a1 1 0 1 0-1-1 1 1 0 0 0-1 1ZM7 14v4h4v-4H7Zm6 0v4h4v-4h-4Z"
        fill="currentColor"
      />
    </svg>
  );
}

function ShareGlyph() {
  return (
    <svg aria-hidden="true" viewBox="0 0 24 24">
      <path
        d="M9.7 10.7l4.6-3.4M9.7 13.3l4.6 3.4"
        fill="none"
        stroke="currentColor"
        strokeLinecap="round"
        strokeLinejoin="round"
        strokeWidth="1.8"
      />
      <circle cx="7" cy="12" r="2.5" fill="none" stroke="currentColor" strokeWidth="1.8" />
      <circle cx="17" cy="6" r="2.5" fill="none" stroke="currentColor" strokeWidth="1.8" />
      <circle cx="17" cy="18" r="2.5" fill="none" stroke="currentColor" strokeWidth="1.8" />
    </svg>
  );
}

function ExternalLinkGlyph() {
  return (
    <svg aria-hidden="true" viewBox="0 0 24 24">
      <path
        d="M14 4h6v6M20 4l-9 9M19 14.5V20H4V5h5.5"
        fill="none"
        stroke="currentColor"
        strokeLinecap="round"
        strokeLinejoin="round"
        strokeWidth="1.9"
      />
    </svg>
  );
}

function MarketTitleLink({
  title,
  marketSlug,
  eventSlug,
}: {
  title: string;
  marketSlug?: string | null;
  eventSlug?: string | null;
}) {
  const url = buildPolymarketMarketUrl(marketSlug, eventSlug);
  return (
    <span className="market-title-link">
      <span className="market-title-link__text">{title}</span>
      {url ? (
        <a
          className="market-title-link__button"
          href={url}
          target="_blank"
          rel="noopener noreferrer"
          title="Open on Polymarket"
          aria-label={`Open ${title} on Polymarket`}
          onClick={(event) => event.stopPropagation()}
        >
          <ExternalLinkGlyph />
        </a>
      ) : null}
    </span>
  );
}

function FeedThumbnail({ src, variant }: { src: string; variant: "activity" | "ledger" }) {
  const [failed, setFailed] = useState(false);
  const shellClass = variant === "activity" ? "activity-feed__thumb" : "home-ledger__thumb";
  const imageClass = variant === "activity" ? "activity-feed__thumb-image" : "home-ledger__thumb-image";

  if (failed) {
    return (
      <div className={`${shellClass} home-feed-thumb-fallback`} aria-hidden="true">
        <span />
      </div>
    );
  }

  return (
    <div className={shellClass}>
      <img src={src} alt="" className={imageClass} loading="lazy" onError={() => setFailed(true)} />
    </div>
  );
}

function PortfolioFeedSkeleton({ tab }: { tab: HomePortfolioTab }) {
  const rowCount = tab === "activity" ? 4 : 3;
  const label = tab.replace("-", " ");

  return (
    <div className={`home-feed-skeleton home-feed-skeleton--${tab}`} role="status" aria-label={`Loading ${label}`}>
      {Array.from({ length: rowCount }).map((_, index) => (
        <div className="home-feed-skeleton__row" key={index}>
          <span className="home-feed-skeleton__marker" aria-hidden="true" />
          <span className="home-feed-skeleton__thumb" aria-hidden="true" />
          <span className="home-feed-skeleton__content" aria-hidden="true">
            <span className="home-skeleton-line home-skeleton-line--medium" />
            <span className="home-skeleton-line home-skeleton-line--short" />
          </span>
          <span className="home-feed-skeleton__aside" aria-hidden="true">
            <span className="home-skeleton-line home-skeleton-line--short" />
            <span className="home-skeleton-line home-skeleton-line--short" />
          </span>
        </div>
      ))}
    </div>
  );
}

function ActivityTab({
  items,
  botState,
  onShareReward,
}: {
  items: HomeActivityItem[];
  botState?: string;
  onShareReward?: (card: PerformanceShareCardPayload) => void;
}) {
  if (items.length === 0) {
    return (
      <div className="empty-state">
        {botState === "running"
          ? "No recent wallet activity yet. Filled buys, sells, rewards, and redemptions will appear here."
          : "No recent wallet activity yet. Start the bot or finish setup first."}
      </div>
    );
  }

  return (
    <div className="activity-feed">
      {items.map((item, index) => {
        const isReward = isRewardActivity(item);
        const title = isReward ? "Rewards distributed for epoch" : item.market_title || item.title || item.message;
        const cashflow = isReward
          ? Math.abs(item.cashflow_usd ?? item.value_usd ?? 0)
          : item.cashflow_usd ?? item.value_usd ?? null;
        const hasPrice = shouldShowActivityPrice(item);
        const outcomeLabel =
          item.outcome && hasPrice ? `${item.outcome} ${formatPriceCents(item.price)}` : item.outcome;
        const actionClass = activityActionClass(item);
        const rewardSubtitle =
          !isReward
            ? null
            : item.detail?.trim() && !item.detail.startsWith("0x")
              ? item.detail.trim()
              : null;
        const rewardShareCard = isReward ? buildRewardActivityShareCard(item) : null;
        return (
          <div
            key={`${item.id}-${item.timestamp}-${index}`}
            className={`activity-feed__row ${
              item.thumbnail_url || isReward ? "activity-feed__row--with-thumb" : ""
            }`.trim()}
          >
            <div className={`activity-feed__action activity-feed__action--${actionClass}`}>
              <div className="activity-feed__marker">
                {actionClass === "sell" ? "-" : actionClass === "reward" ? "$" : "+"}
              </div>
              <div className="activity-feed__action-label">{isReward ? "Reward" : item.action ?? "Activity"}</div>
            </div>

            {item.thumbnail_url && !isReward ? (
              <FeedThumbnail src={item.thumbnail_url} variant="activity" />
            ) : isReward ? (
              <div className="activity-feed__thumb activity-feed__thumb--reward">
                <RewardIcon />
              </div>
            ) : null}

            <div className="activity-feed__content">
              <div className="activity-feed__title">
                <MarketTitleLink title={title} marketSlug={item.market_slug} eventSlug={item.event_slug} />
              </div>
              <div className="activity-feed__meta">
                {outcomeLabel && !isReward ? (
                  <span className={`activity-feed__chip activity-feed__chip--${activityOutcomeClass(item.outcome)}`}>
                    {outcomeLabel}
                  </span>
                ) : hasPrice && !isReward ? (
                  <span>@ {formatPriceCents(item.price)}</span>
                ) : null}
                {typeof item.quantity === "number" && Number.isFinite(item.quantity) && !isReward ? (
                  <span>{formatControlValue(item.quantity)} shares</span>
                ) : null}
                {isReward ? (
                  rewardSubtitle ? <span>{rewardSubtitle}</span> : null
                ) : item.detail ? (
                  <span>{item.detail}</span>
                ) : null}
              </div>
            </div>

            <div className="activity-feed__aside">
              {typeof cashflow === "number" && Number.isFinite(cashflow) ? (
                <div className={`activity-feed__value activity-feed__value--${activityValueClass(cashflow)}`}>
                  {formatMoney(cashflow)}
                </div>
              ) : null}
              <div className="activity-feed__time">{formatRelativeTime(item.timestamp)}</div>
              {rewardShareCard && onShareReward ? (
                <button
                  type="button"
                  className="performance-share-inline"
                  onClick={() => onShareReward(rewardShareCard)}
                  title="Share liquidity reward"
                >
                  <ShareGlyph />
                  <span>Share</span>
                </button>
              ) : null}
            </div>
          </div>
        );
      })}
    </div>
  );
}

function PositionsTab({
  items,
  walletAddress,
  onSharePosition,
}: {
  items: HomePositionItem[];
  walletAddress?: string | null;
  onSharePosition?: (card: PerformanceShareCardPayload) => void;
}) {
  if (items.length === 0) {
    return (
      <div className="empty-state">
        {walletAddress
          ? "No open positions found for the active wallet."
          : "Open positions appear after a wallet is linked to this bot."}
      </div>
    );
  }

  return (
    <div className="home-ledger">
      <div className="home-ledger__header home-ledger__header--positions">
        <div>Market</div>
        <div>AVG -&gt; NOW</div>
        <div>Traded</div>
        <div>To Win</div>
        <div>Value</div>
      </div>
      <div className="home-ledger__list">
        {items.map((item, index) => {
          const traded = getPositionTraded(item);
          const pnl = getPositionPnl(item);
          const pnlPercent = getPositionPercentPnl(item);
          const currentValue = item.current_value ?? traded;
          const valueTone = activityValueClass(pnl);
          const positionShareCard = buildPositionMoveShareCard(item);

          return (
            <div
              key={`${item.condition_id ?? item.token_id ?? "position"}-${index}`}
              className="home-ledger__row home-ledger__row--positions"
            >
              <div className="home-ledger__market">
                {item.thumbnail_url ? <FeedThumbnail src={item.thumbnail_url} variant="ledger" /> : null}
                <div className="home-ledger__market-copy">
                  <div className="home-ledger__market-title">
                    <MarketTitleLink
                      title={item.market_title || "Unknown market"}
                      marketSlug={item.market_slug}
                      eventSlug={item.event_slug}
                    />
                  </div>
                  <div className="home-ledger__market-meta">
                    {item.outcome ? (
                      <span className={`activity-feed__chip activity-feed__chip--${outcomeTone(item.outcome)}`}>
                        {item.outcome}
                        {typeof item.avg_price === "number" ? ` ${formatPriceCents(item.avg_price)}` : ""}
                      </span>
                    ) : null}
                    {typeof item.size === "number" && Number.isFinite(item.size) ? (
                      <span>{formatControlValue(item.size)} shares</span>
                    ) : null}
                    {item.redeemable ? <span>Redeemable</span> : null}
                  </div>
                </div>
              </div>
              <div className="home-ledger__metric">{`${formatPriceCents(item.avg_price)} -> ${formatPriceCents(
                item.current_price,
              )}`}</div>
              <div className="home-ledger__metric">{formatMoney(traded)}</div>
              <div className="home-ledger__metric">{formatMoney(item.size)}</div>
              <div className="home-ledger__value">
                <div className="home-ledger__value-primary">{formatMoney(currentValue)}</div>
                {pnl !== null ? (
                  <div className={`home-ledger__value-secondary home-ledger__value-secondary--${valueTone}`}>
                    {formatMoney(pnl)}
                    {pnlPercent !== null ? ` (${formatPercent(pnlPercent)})` : ""}
                  </div>
                ) : null}
                {positionShareCard && onSharePosition ? (
                  <button
                    type="button"
                    className="performance-share-inline performance-share-inline--value"
                    onClick={() => onSharePosition(positionShareCard)}
                    title="Share position move"
                  >
                    <ShareGlyph />
                    <span>Share</span>
                  </button>
                ) : null}
              </div>
            </div>
          );
        })}
      </div>
    </div>
  );
}

function OpenOrdersTab({
  groups,
  workerAvailable,
  transientRecovery,
}: {
  groups: OpenOrderGroup[];
  workerAvailable: boolean;
  transientRecovery?: boolean;
}) {
  const [expandedKeys, setExpandedKeys] = useState<Record<string, boolean>>({});

  if (groups.length === 0) {
    return (
      <div className="empty-state">
        {transientRecovery
          ? "Refreshing worker data after a short handoff. Open orders should return automatically."
          : workerAvailable
          ? "No open orders found for the active wallet."
          : "Open orders are only available while this bot is running on a worker."}
      </div>
    );
  }

  return (
    <div className="home-ledger">
      <div className="home-ledger__header home-ledger__header--orders">
        <div>Market</div>
        <div>Filled</div>
        <div>Total</div>
        <div>Expiration</div>
      </div>
      <div className="home-ledger__list">
        {groups.map((group) => {
          const expanded = Boolean(expandedKeys[group.key]);
          return (
            <div key={group.key} className="home-ledger__group">
              <div className="home-ledger__row home-ledger__row--orders">
                <div className="home-ledger__market">
                  {group.thumbnailUrl ? <FeedThumbnail src={group.thumbnailUrl} variant="ledger" /> : null}
                  <div className="home-ledger__market-copy">
                    <div className="home-ledger__market-title">
                      <MarketTitleLink
                        title={group.marketTitle}
                        marketSlug={group.marketSlug}
                        eventSlug={group.eventSlug}
                      />
                    </div>
                    <div className="home-ledger__market-meta">
                      <span>{group.orders.length} orders</span>
                      <button
                        type="button"
                        className="home-ledger__details-toggle"
                        aria-expanded={expanded}
                        onClick={() =>
                          setExpandedKeys((current) => ({
                            ...current,
                            [group.key]: !expanded,
                          }))
                        }
                      >
                        {expanded ? "Hide details" : "Show details"}
                      </button>
                    </div>
                  </div>
                </div>
                <div className="home-ledger__metric">
                  {formatControlValue(group.totalMatched)} / {formatControlValue(group.totalOriginal)}
                </div>
                <div className="home-ledger__metric">{formatMoney(group.totalNotionalUsd)}</div>
                <div className="home-ledger__metric">{formatRelativeTime(group.nearestExpiration)}</div>
              </div>
              {expanded ? (
                <div className="home-ledger__details">
                  {group.orders.map((order) => (
                    <div key={order.id} className="home-ledger__detail-row">
                      <div className="home-ledger__detail-left">
                        <span className={`activity-feed__chip activity-feed__chip--${activityOutcomeClass(order.outcome)}`}>
                          {order.outcome || order.side || "Order"}
                        </span>
                        <span className="home-ledger__detail-text">
                          {(order.side || "Order").toUpperCase()} {formatControlValue(order.remaining_size ?? order.original_size ?? 0)} @{" "}
                          {formatPriceCents(order.price)}
                        </span>
                      </div>
                      <div className="home-ledger__detail-right">
                        <span>{formatMoney(order.total_notional_usd)}</span>
                        <span>{formatRelativeTime(order.expiration)}</span>
                      </div>
                    </div>
                  ))}
                </div>
              ) : null}
            </div>
          );
        })}
      </div>
    </div>
  );
}

function formatSeriesHeroValue(
  key: PerformanceChartSeriesKey,
  value: number | null,
  partial = false,
): string {
  if (typeof value !== "number" || !Number.isFinite(value)) {
    return "--";
  }
  const formatted = key === "pnl" ? formatSignedMoney(value) : formatMoney(value);
  return partial ? `>${formatted}` : formatted;
}

function getSeriesTone(
  key: PerformanceChartSeriesKey,
  value: number | null,
): "positive" | "negative" | "neutral" | "blue" | "orange" {
  if (key === "pnl") {
    return valueToneClass(value);
  }
  if (key === "maker") {
    return "blue";
  }
  if (key === "lp") {
    return "positive";
  }
  return "orange";
}

function PerformanceChart({
  pnlPoints,
  inlaySeries,
  activeSeries,
  onToggleSeries,
  workerRunning,
}: {
  pnlPoints: HomePerformanceSnapshot["series"];
  inlaySeries: Array<{
    key: PerformanceChartSeriesKey;
    label: string;
    color: string;
    value: string;
    tone: "positive" | "negative" | "neutral" | "blue" | "orange";
  }>;
  activeSeries: Set<PerformanceChartSeriesKey>;
  onToggleSeries: (key: PerformanceChartSeriesKey) => void;
  workerRunning: boolean;
}) {
  const fillId = useRef(`performanceChartFill-${Math.random().toString(36).slice(2, 9)}`).current;
  const pnlPath = buildSingleChartPath({
    key: "pnl",
    color: "var(--green)",
    fill: true,
    points: pnlPoints,
  });

  return (
    <div className="performance-charts">
      <div className="performance-charts__legend" aria-label="Chart series">
        {PERFORMANCE_CHART_SERIES.map((series) => {
          const active = activeSeries.has(series.key);
          return (
            <button
              key={series.key}
              type="button"
              className={`performance-chart__legend-item ${active ? "performance-chart__legend-item--active" : ""}`.trim()}
              aria-pressed={active}
              onClick={() => onToggleSeries(series.key)}
              disabled={series.key === "pnl"}
            >
              <span className="performance-chart__legend-dot" style={{ background: series.color }} aria-hidden="true" />
              {series.label}
            </button>
          );
        })}
      </div>
      <div className="performance-chart performance-chart--pnl-only">
        {pnlPath ? (
          <>
            <div className="performance-chart__inlay-legend" aria-label="Selected performance metrics">
              {inlaySeries.map((series) => (
                <div key={series.key} className={`performance-chart__inlay-item performance-chart__inlay-item--${series.key}`}>
                  <span className="performance-chart__inlay-dot" style={{ background: series.color }} aria-hidden="true" />
                  <span className="performance-chart__inlay-label">{series.label}</span>
                  <strong className={`performance-chart__inlay-value performance-value--${series.tone}`}>
                    {series.value}
                  </strong>
                </div>
              ))}
            </div>
            <svg viewBox="0 0 640 260" preserveAspectRatio="none" aria-hidden="true">
              <defs>
                <linearGradient id={fillId} x1="0" x2="0" y1="0" y2="1">
                  <stop offset="0%" stopColor="currentColor" stopOpacity="0.22" />
                  <stop offset="100%" stopColor="currentColor" stopOpacity="0" />
                </linearGradient>
              </defs>
              <g className="performance-chart__grid">
                <path d="M18 48H622M18 96H622M18 144H622M18 192H622M18 230H622" />
                <path d="M110 18V230M210 18V230M310 18V230M410 18V230M510 18V230" />
              </g>
              <g className="performance-chart__layer performance-chart__layer--pnl" style={{ color: "var(--green)" }}>
                <path className="performance-chart__area" d={pnlPath.area} fill={`url(#${fillId})`} />
                <path className="performance-chart__line" d={pnlPath.line} />
              </g>
            </svg>
          </>
        ) : (
          <div className="performance-chart__empty">
            {workerRunning
              ? "PnL chart is syncing from Polymarket."
              : "Showing latest stored snapshot while this bot is stopped."}
          </div>
        )}
      </div>
    </div>
  );
}

type CalendarCell = {
  key: string;
  day: number | null;
  value: number | null;
  stat: HomePerformanceSnapshot["dailyStats"][number] | null;
};

function buildCalendarCells(snapshot: HomePerformanceSnapshot): CalendarCell[] {
  const anchor = snapshot.pnlAsOfUtc ? new Date(snapshot.pnlAsOfUtc) : new Date();
  const year = anchor.getUTCFullYear();
  const month = anchor.getUTCMonth();
  const today = anchor.getUTCDate();
  const firstDow = new Date(Date.UTC(year, month, 1)).getUTCDay();
  const daysInMonth = new Date(Date.UTC(year, month + 1, 0)).getUTCDate();
  const stats = new Map<number, HomePerformanceSnapshot["dailyStats"][number]>();

  for (const stat of snapshot.dailyStats) {
    const parsed = new Date(`${stat.date}T00:00:00.000Z`);
    if (parsed.getUTCFullYear() === year && parsed.getUTCMonth() === month) {
      stats.set(parsed.getUTCDate(), stat);
    }
  }

  if (stats.size === 0 && snapshot.series.length > 1) {
    const grouped = new Map<string, { first: number; last: number }>();
    for (const point of snapshot.series) {
      const dayKey = point.ts.slice(0, 10);
      const current = grouped.get(dayKey);
      if (!current) {
        grouped.set(dayKey, { first: point.value, last: point.value });
      } else {
        current.last = point.value;
      }
    }
    for (const [dayKey, group] of grouped.entries()) {
      const parsed = new Date(`${dayKey}T00:00:00.000Z`);
      if (parsed.getUTCFullYear() === year && parsed.getUTCMonth() === month) {
        stats.set(parsed.getUTCDate(), {
          date: dayKey,
          pnl: group.last - group.first,
          volume: null,
          makerRebate: null,
          lpRewards: null,
          trades: 0,
        });
      }
    }
  }

  if (!stats.has(today) && typeof snapshot.pnl === "number" && Number.isFinite(snapshot.pnl)) {
    stats.set(today, {
      date: `${year}-${String(month + 1).padStart(2, "0")}-${String(today).padStart(2, "0")}`,
      pnl: snapshot.pnl,
      volume: null,
      makerRebate: null,
      lpRewards: null,
      trades: 0,
    });
  }

  const cells: CalendarCell[] = [];
  for (let index = 0; index < firstDow; index += 1) {
    cells.push({ key: `blank-${index}`, day: null, value: null, stat: null });
  }
  for (let day = 1; day <= daysInMonth; day += 1) {
    const stat = stats.get(day) ?? null;
    cells.push({
      key: `day-${day}`,
      day,
      value: stat?.pnl ?? null,
      stat,
    });
  }
  while (cells.length % 7 !== 0) {
    cells.push({ key: `tail-${cells.length}`, day: null, value: null, stat: null });
  }
  return cells;
}

function getDefaultSelectedDate(cells: CalendarCell[]): string | null {
  for (let index = cells.length - 1; index >= 0; index -= 1) {
    const stat = cells[index]?.stat ?? null;
    if (stat) {
      return stat.date;
    }
  }
  return null;
}

function PerformanceCalendar({ snapshot }: { snapshot: HomePerformanceSnapshot }) {
  const cells = buildCalendarCells(snapshot);
  const defaultSelectedDate = getDefaultSelectedDate(cells);
  const [selectedDate, setSelectedDate] = useState<string | null>(defaultSelectedDate);
  useEffect(() => {
    setSelectedDate(defaultSelectedDate);
  }, [defaultSelectedDate]);
  const anchor = snapshot.pnlAsOfUtc ? new Date(snapshot.pnlAsOfUtc) : new Date();
  const monthLabel = new Intl.DateTimeFormat("en-US", {
    month: "long",
    timeZone: "UTC",
  }).format(anchor);
  const monthTotal = cells.reduce((sum, cell) => sum + (cell.value ?? 0), 0);
  const wins = cells.filter((cell) => typeof cell.value === "number" && cell.value > 0).length;
  const losses = cells.filter((cell) => typeof cell.value === "number" && cell.value < 0).length;
  const best = cells.reduce<number | null>(
    (current, cell) =>
      typeof cell.value === "number" && (current === null || cell.value > current) ? cell.value : current,
    null,
  );
  const worst = cells.reduce<number | null>(
    (current, cell) =>
      typeof cell.value === "number" && (current === null || cell.value < current) ? cell.value : current,
    null,
  );
  const selectedStat =
    cells.find((cell) => cell.stat?.date === selectedDate)?.stat ??
    cells.find((cell) => cell.stat)?.stat ??
    null;
  const selectedLabel = selectedStat
    ? new Intl.DateTimeFormat("en-US", {
        month: "short",
        day: "numeric",
        timeZone: "UTC",
      }).format(new Date(`${selectedStat.date}T00:00:00.000Z`))
    : null;

  return (
    <div className="performance-calendar">
      <div className="performance-section-head">
        <div>
          <h3>{monthLabel}</h3>
          <div className={`performance-section-head__value performance-value--${valueToneClass(monthTotal)}`}>
            {formatSignedMoney(monthTotal)}
          </div>
        </div>
        <div className="performance-calendar__mode">Daily</div>
      </div>
      <div className="performance-calendar__stats">
        <span>
          Wins <strong>{wins}</strong>
        </span>
        <span>
          Losses <strong>{losses}</strong>
        </span>
        <span>
          Best <strong className="performance-value--positive">{formatSignedMoney(best)}</strong>
        </span>
        <span>
          Worst <strong className="performance-value--negative">{formatSignedMoney(worst)}</strong>
        </span>
      </div>
      <div className="performance-calendar__grid">
        {["S", "M", "T", "W", "T", "F", "S"].map((day) => (
          <div key={day} className="performance-calendar__dow">
            {day}
          </div>
        ))}
        {cells.map((cell) => {
          const tone = valueToneClass(cell.value);
          return (
            <button
              type="button"
              key={cell.key}
              className={`performance-calendar__day performance-calendar__day--${tone} ${
                cell.day === null ? "performance-calendar__day--blank" : ""
              } ${cell.stat?.date === selectedDate ? "performance-calendar__day--selected" : ""} ${
                cell.stat ? "" : "performance-calendar__day--disabled"
              }`.trim()}
              disabled={!cell.stat}
              onClick={() => {
                if (cell.stat) {
                  setSelectedDate(cell.stat.date);
                }
              }}
            >
              <span>{cell.day ?? ""}</span>
              {cell.value !== null ? <strong>{formatSignedMoney(cell.value)}</strong> : null}
            </button>
          );
        })}
      </div>
      <div className="performance-calendar__selected">
        <div>
          <span>{selectedLabel ?? "Selected day"}</span>
          <strong className={`performance-value--${valueToneClass(selectedStat?.pnl)}`}>
            {formatSignedMoney(selectedStat?.pnl)}
          </strong>
        </div>
        <div>
          <span>Volume</span>
          <strong>{formatMoney(selectedStat?.volume)}</strong>
        </div>
        <div>
          <span>Maker Rebate</span>
          <strong className="performance-value--blue">{formatMoney(selectedStat?.makerRebate)}</strong>
        </div>
        <div>
          <span>LP Rewards</span>
          <strong className="performance-value--positive">{formatMoney(selectedStat?.lpRewards)}</strong>
        </div>
        <div>
          <span>Trades</span>
          <strong>{formatCount(selectedStat?.trades)}</strong>
        </div>
      </div>
    </div>
  );
}

function getSeriesHeroValue(
  key: PerformanceChartSeriesKey,
  snapshot: HomePerformanceSnapshot,
  activeRange: ProfilePerformanceRange,
  activeWindow: HomePerformanceSnapshot["windows"][ProfilePerformanceRange],
): number | null {
  const asOf = snapshot.pnlAsOfUtc ? new Date(snapshot.pnlAsOfUtc) : new Date();
  const rangeStartMs = getPerformanceRangeStartMs(activeRange, asOf);

  switch (key) {
    case "pnl":
      return activeWindow.pnl;
    case "volume":
      if (activeRange === "6h" || activeRange === "1d") {
        return null;
      }
      return activeRange === "all"
        ? snapshot.allTimeVolume
        : sumDailyStatInRange(snapshot.dailyStats, rangeStartMs, (stat) => stat.volume);
    case "maker":
      if (activeRange === "6h" || activeRange === "1d") {
        return null;
      }
      return activeRange === "all"
        ? snapshot.makerRebateLifetime
        : sumDailyStatInRange(snapshot.dailyStats, rangeStartMs, (stat) => stat.makerRebate);
    case "lp":
      if (activeRange === "6h" || activeRange === "1d") {
        return null;
      }
      return activeRange === "all"
        ? snapshot.lpRewardsLifetime
        : sumDailyStatInRange(snapshot.dailyStats, rangeStartMs, (stat) => stat.lpRewards);
  }
}

function PerformanceTab({ snapshot }: { snapshot: HomePerformanceSnapshot }) {
  const [activeRange, setActiveRange] = useState<ProfilePerformanceRange>("1d");
  const [activeSeries, setActiveSeries] = useState<Set<PerformanceChartSeriesKey>>(() => new Set(["pnl"]));

  useEffect(() => {
    setActiveRange("1d");
    setActiveSeries(new Set(["pnl"]));
  }, [snapshot.profileName, snapshot.walletAddress]);

  const activeWindow = snapshot.windows[activeRange] ?? snapshot.windows["1d"];
  const pnlChartPoints = activeWindow.series;

  const inlaySeries = useMemo(
    () =>
      PERFORMANCE_CHART_SERIES.filter((series) => activeSeries.has(series.key)).map((series) => {
        const value = getSeriesHeroValue(series.key, snapshot, activeRange, activeWindow);
        const partial =
          activeRange === "all" &&
          ((series.key === "volume" && snapshot.allTimeVolumePartial) ||
            (series.key === "maker" && snapshot.makerRebateLifetimePartial));
        return {
          key: series.key,
          label: series.label,
          color: series.color,
          value: formatSeriesHeroValue(series.key, value, partial),
          tone: getSeriesTone(series.key, value),
        };
      }),
    [activeSeries, snapshot, activeRange, activeWindow],
  );

  const toggleSeries = (key: PerformanceChartSeriesKey) => {
    if (key === "pnl") {
      return;
    }
    setActiveSeries((current) => {
      const next = new Set(current);
      if (next.has(key)) {
        next.delete(key);
      } else {
        next.add(key);
      }
      next.add("pnl");
      return next;
    });
  };

  return (
    <div className="performance-tab">
      <div className="performance-tab__main">
        <div className="performance-panel performance-panel--chart">
          <div className="performance-section-head">
            <div>
              <h3>Performance Over Time</h3>
              <p>
                Source {snapshot.pnlSourceLabel} | Feed {snapshot.pnlFeedLabel}
              </p>
            </div>
            <div className="performance-range-tabs" aria-label="Performance chart range">
              {PERFORMANCE_RANGE_OPTIONS.map((option) => (
                <button
                  type="button"
                  key={option.range}
                  className={activeRange === option.range ? "performance-range-tabs__button--active" : ""}
                  onClick={() => setActiveRange(option.range)}
                  aria-pressed={activeRange === option.range}
                >
                  {option.label}
                </button>
              ))}
            </div>
          </div>
          <PerformanceChart
            pnlPoints={pnlChartPoints}
            inlaySeries={inlaySeries}
            activeSeries={activeSeries}
            onToggleSeries={toggleSeries}
            workerRunning={snapshot.workerRunning}
          />
        </div>

        <div className="performance-panel performance-panel--calendar">
          <PerformanceCalendar snapshot={snapshot} />
        </div>
      </div>
    </div>
  );
}

export function HomePortfolioTabs({
  profileId,
  walletAddress,
  workerAvailable,
  botState,
  refreshToken = 0,
  onOpenLogs,
  onSharePosition,
  onShareReward,
  performanceSnapshot,
  onTabChange,
}: {
  profileId: string | null;
  walletAddress: string | null;
  workerAvailable: boolean;
  botState?: string;
  refreshToken?: number;
  onOpenLogs: () => void;
  onSharePosition?: (card: PerformanceShareCardPayload) => void;
  onShareReward?: (card: PerformanceShareCardPayload) => void;
  performanceSnapshot: HomePerformanceSnapshot;
  onTabChange?: (tab: HomePortfolioTab) => void;
}) {
  const requestSequence = useRef(0);
  const initialActivityDeferRef = useRef(false);
  const [tab, setTab] = useState<HomePortfolioTab>("activity");
  const [search, setSearch] = useState("");
  const [activity, setActivity] = useState<HomeTabState<HomeActivityItem>>(EMPTY_TAB_STATE);
  const [positions, setPositions] = useState<HomeTabState<HomePositionItem>>(EMPTY_TAB_STATE);
  const [openOrders, setOpenOrders] = useState<HomeTabState<HomeOpenOrderItem>>(EMPTY_TAB_STATE);
  const [openOrdersRecoverySeed, setOpenOrdersRecoverySeed] = useState(0);

  useEffect(() => {
    onTabChange?.(tab);
  }, [onTabChange, tab]);

  useEffect(() => {
    setTab("activity");
    setSearch("");
    setActivity(EMPTY_TAB_STATE);
    setPositions(EMPTY_TAB_STATE);
    setOpenOrders(EMPTY_TAB_STATE);
    setOpenOrdersRecoverySeed(0);
    initialActivityDeferRef.current = false;
  }, [profileId]);

  useEffect(() => {
    if (!profileId) {
      return;
    }

    let cancelled = false;
    let controller: AbortController | null = null;
    let timer: number | null = null;
    let interval: number | null = null;
    const activeTab = tab;

    const setLoadingState = () => {
      if (activeTab === "activity") {
        setActivity((current) => ({
          ...current,
          isLoading: true,
        }));
      } else if (activeTab === "positions") {
        setPositions((current) => ({
          ...current,
          isLoading: true,
        }));
      } else {
        setOpenOrders((current) => ({
          ...current,
          isLoading: true,
        }));
      }
    };

    const setResultState = (items: HomeActivityItem[] | HomePositionItem[] | HomeOpenOrderItem[], error: string | null) => {
      const nextState = {
        items,
        error,
        isLoading: false,
        loaded: true,
      };
      if (activeTab === "activity") {
        setActivity(nextState as HomeTabState<HomeActivityItem>);
      } else if (activeTab === "positions") {
        setPositions(nextState as HomeTabState<HomePositionItem>);
      } else {
        setOpenOrders(nextState as HomeTabState<HomeOpenOrderItem>);
      }
    };

    const load = async () => {
      if (cancelled || !isDocumentVisible()) {
        return;
      }
      if (activeTab === "performance") {
        return;
      }

      const requestId = ++requestSequence.current;
      controller = new AbortController();
      setLoadingState();

      try {
        if (activeTab === "activity") {
          const items = await getHomeActivityApi(30);
          const view = { items: items ?? [], error: null };
          if (!cancelled && requestId === requestSequence.current && !controller.signal.aborted) {
            setResultState(view.items, view.error);
          }
          return;
        }

        if (activeTab === "positions") {
          const items = await getHomePositionsApi(80);
          const view = { items: items ?? [], error: null };
          if (!cancelled && requestId === requestSequence.current && !controller.signal.aborted) {
            setResultState(view.items, view.error);
          }
          return;
        }

        if (!workerAvailable) {
          if (!cancelled && requestId === requestSequence.current && !controller.signal.aborted) {
            setResultState([], null);
          }
          return;
        }

        const items = await getHomeOpenOrdersApi(120);
        const view = { items: items ?? [], error: null };
        if (!cancelled && requestId === requestSequence.current && !controller.signal.aborted) {
          setResultState(view.items, view.error);
        }
      } catch (error) {
        if (!cancelled && requestId === requestSequence.current && !isAbortError(error)) {
          setResultState([], readErrorMessage(error));
        }
      } finally {
        controller = null;
      }
    };
    const shouldDeferActivityLoad = activeTab === "activity" && !initialActivityDeferRef.current;
    if (shouldDeferActivityLoad) {
      initialActivityDeferRef.current = true;
    }
    timer = window.setTimeout(
      () => {
        void load();
      },
      shouldDeferActivityLoad ? INITIAL_ACTIVITY_LOAD_DELAY_MS : 0,
    );
    if (activeTab !== "performance") {
      interval = window.setInterval(() => {
        void load();
      }, TAB_REFRESH_INTERVAL_MS[activeTab]);
    }

    return () => {
      cancelled = true;
      if (timer !== null) {
        window.clearTimeout(timer);
      }
      if (interval !== null) {
        window.clearInterval(interval);
      }
      requestSequence.current += 1;
      controller?.abort();
    };
  }, [openOrdersRecoverySeed, profileId, refreshToken, tab, walletAddress, workerAvailable]);

  useEffect(() => {
    if (
      tab !== "open-orders" ||
      !profileId ||
      !workerAvailable ||
      openOrders.isLoading ||
      !isTransientWorkerRecoveryError(openOrders.error)
    ) {
      return;
    }

    const timer = window.setTimeout(() => {
      setOpenOrdersRecoverySeed((current) => current + 1);
    }, 4000);

    return () => {
      window.clearTimeout(timer);
    };
  }, [openOrders.error, openOrders.isLoading, profileId, tab, workerAvailable]);

  const query = search.trim().toLowerCase();

  const filteredActivity = useMemo(
    () =>
      [...activity.items]
        .sort((left, right) => right.timestamp.localeCompare(left.timestamp))
        .filter((item) => {
          if (!query) return true;
          return [item.market_title, item.title, item.outcome, item.action, item.message, item.detail]
            .filter(Boolean)
            .some((value) => value!.toLowerCase().includes(query));
        }),
    [activity.items, query],
  );

  const filteredPositions = useMemo(
    () =>
      [...positions.items]
        .filter((item) => {
          if (!query) return true;
          return [item.market_title, item.market_slug, item.event_slug, item.outcome, item.opposite_outcome]
            .filter(Boolean)
            .some((value) => value!.toLowerCase().includes(query));
        })
        .sort((left, right) => (right.current_value ?? 0) - (left.current_value ?? 0)),
    [positions.items, query],
  );

  const filteredOpenOrderGroups = useMemo(
    () =>
      groupOpenOrders(
        openOrders.items.filter((item) => {
          if (!query) return true;
          return [item.market_title, item.outcome, item.side, item.id]
            .filter(Boolean)
            .some((value) => value!.toLowerCase().includes(query));
        }),
      ),
    [openOrders.items, query],
  );

  const activeState =
    tab === "activity"
      ? activity
      : tab === "positions"
        ? positions
        : tab === "open-orders"
          ? openOrders
          : EMPTY_TAB_STATE;
  const transientWorkerRecovery =
    tab === "open-orders" && isTransientWorkerRecoveryError(openOrders.error);
  const displayError = tab === "performance" || transientWorkerRecovery ? null : activeState.error;
  const activeStatePending = tab !== "performance" && !activeState.loaded && !displayError && !transientWorkerRecovery;
  const activePlaceholder =
    tab === "activity"
      ? "Search activity"
      : tab === "positions"
        ? "Search positions"
        : tab === "open-orders"
          ? "Search open orders"
          : "";

  return (
    <SectionPanel
      title="Portfolio Feed"
      subtitle="Live wallet activity, open positions, open orders, and performance from the active trading wallet."
      actions={
        <button type="button" onClick={onOpenLogs} className="ui-button ui-button--compact">
          Open Logs
        </button>
      }
    >
      <div className="home-portfolio-tabs">
        <div
          className="segmented-control home-portfolio-tabs__segmented"
          role="tablist"
          aria-label="Portfolio tabs"
        >
          {[
            ["activity", "Activity"],
            ["positions", "Positions"],
            ["open-orders", "Open Orders"],
            ["performance", "Performance"],
          ].map(([value, label]) => {
            const active = tab === value;
            return (
              <button
                key={value}
                type="button"
                role="tab"
                aria-selected={active}
                onClick={() => setTab(value as HomePortfolioTab)}
                className={`segmented-control__option ${
                  active ? "segmented-control__option--active" : ""
                }`.trim()}
              >
                {label}
              </button>
            );
          })}
        </div>

        {tab !== "performance" ? (
          <div className="home-portfolio-tabs__toolbar">
            <label className="home-portfolio-tabs__search">
              <SearchIcon />
              <input
                value={search}
                onChange={(event) => setSearch(event.target.value)}
                placeholder={activePlaceholder}
                className="field-input field-input--compact home-portfolio-tabs__search-input"
              />
            </label>
          </div>
        ) : null}

        {transientWorkerRecovery ? (
          <div className="status-strip">
            <div className="status-strip__title">Refreshing worker data</div>
            <div className="status-strip__copy">
              This bot is reconnecting after a short worker handoff. Open orders should return automatically in a few
              moments.
            </div>
          </div>
        ) : null}

        {displayError ? <div className="inline-alert inline-alert--warning">{displayError}</div> : null}

        {tab === "performance" ? (
          <PerformanceTab snapshot={performanceSnapshot} />
        ) : activeStatePending ? (
          <PortfolioFeedSkeleton tab={tab} />
        ) : tab === "activity" ? (
          <ActivityTab items={filteredActivity} botState={botState} onShareReward={onShareReward} />
        ) : tab === "positions" ? (
          <PositionsTab items={filteredPositions} walletAddress={walletAddress} onSharePosition={onSharePosition} />
        ) : (
          <OpenOrdersTab
            groups={filteredOpenOrderGroups}
            workerAvailable={workerAvailable}
            transientRecovery={transientWorkerRecovery}
          />
        )}
      </div>
    </SectionPanel>
  );
}
