import { useEffect, useMemo, useRef, useState } from "react";
import { open } from "@tauri-apps/plugin-shell";
import { SectionPanel } from "./SectionPanel";
import type {
  HomeActivityItem,
  HomeOpenOrderItem,
  HomePositionItem,
} from "../lib/platform-api";
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

function withTimeout<T>(promise: Promise<T>, timeoutMs: number, label: string): Promise<T> {
  let timeoutId: number | null = null;
  const timeout = new Promise<never>((_, reject) => {
    timeoutId = window.setTimeout(() => reject(new Error(`${label} timed out`)), timeoutMs);
  });
  return Promise.race([promise, timeout]).finally(() => {
    if (timeoutId !== null) {
      window.clearTimeout(timeoutId);
    }
  });
}

type HomePortfolioTab = "activity" | "positions" | "open-orders";

const INITIAL_ACTIVITY_LOAD_DELAY_MS = 250;
const TAB_REQUEST_TIMEOUT_MS = 8_000;
const OPEN_ORDERS_REQUEST_TIMEOUT_MS = 22_000;
const TAB_REFRESH_INTERVAL_MS: Record<HomePortfolioTab, number> = {
  activity: 15_000,
  positions: 30_000,
  "open-orders": 30_000,
};

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
        <button
          type="button"
          className="market-title-link__button"
          title="Open on Polymarket"
          aria-label={`Open ${title} on Polymarket`}
          onClick={(event) => {
            event.stopPropagation();
            void open(url);
          }}
        >
          <ExternalLinkGlyph />
        </button>
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
}: {
  groups: OpenOrderGroup[];
}) {
  const [expandedKeys, setExpandedKeys] = useState<Record<string, boolean>>({});

  if (groups.length === 0) {
    return (
      <div className="empty-state">
        No open orders found for the active wallet.
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

export function HomePortfolioTabs({
  profileId,
  walletAddress,
  botState,
  refreshToken = 0,
  onOpenLogs,
  onSharePosition,
  onShareReward,
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
  onTabChange?: (tab: HomePortfolioTab) => void;
}) {
  const requestSequence = useRef(0);
  const initialActivityDeferRef = useRef(false);
  const [tab, setTab] = useState<HomePortfolioTab>("activity");
  const [search, setSearch] = useState("");
  const [activity, setActivity] = useState<HomeTabState<HomeActivityItem>>(EMPTY_TAB_STATE);
  const [positions, setPositions] = useState<HomeTabState<HomePositionItem>>(EMPTY_TAB_STATE);
  const [openOrders, setOpenOrders] = useState<HomeTabState<HomeOpenOrderItem>>(EMPTY_TAB_STATE);

  useEffect(() => {
    onTabChange?.(tab);
  }, [onTabChange, tab]);

  useEffect(() => {
    setTab("activity");
    setSearch("");
    setActivity(EMPTY_TAB_STATE);
    setPositions(EMPTY_TAB_STATE);
    setOpenOrders(EMPTY_TAB_STATE);
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
    let inFlight = false;
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
      if (cancelled || inFlight || !isDocumentVisible()) {
        return;
      }
      inFlight = true;
      const requestId = ++requestSequence.current;
      controller = new AbortController();
      setLoadingState();

      try {
        if (activeTab === "activity") {
          const items = await withTimeout(getHomeActivityApi(30), TAB_REQUEST_TIMEOUT_MS, "Activity load");
          const view = { items: items ?? [], error: null };
          if (!cancelled && requestId === requestSequence.current && !controller.signal.aborted) {
            setResultState(view.items, view.error);
          }
          return;
        }

        if (activeTab === "positions") {
          const items = await withTimeout(getHomePositionsApi(80), TAB_REQUEST_TIMEOUT_MS, "Positions load");
          const view = { items: items ?? [], error: null };
          if (!cancelled && requestId === requestSequence.current && !controller.signal.aborted) {
            setResultState(view.items, view.error);
          }
          return;
        }

        const items = await withTimeout(
          getHomeOpenOrdersApi(120),
          OPEN_ORDERS_REQUEST_TIMEOUT_MS,
          "Open orders load",
        );
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
        inFlight = false;
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
    interval = window.setInterval(() => {
      void load();
    }, TAB_REFRESH_INTERVAL_MS[activeTab]);

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
  }, [profileId, refreshToken, tab, walletAddress]);

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
        : openOrders;
  const displayError = activeState.error;
  const activeStatePending = !activeState.loaded && !displayError;
  const activePlaceholder =
    tab === "activity"
      ? "Search activity"
      : tab === "positions"
        ? "Search positions"
        : "Search open orders";

  return (
    <SectionPanel
      title="Portfolio Feed"
      subtitle="Live wallet activity, open positions, and open orders from the active trading wallet."
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

        {displayError ? <div className="inline-alert inline-alert--warning">{displayError}</div> : null}

        {activeStatePending ? (
          <PortfolioFeedSkeleton tab={tab} />
        ) : tab === "activity" ? (
          <ActivityTab items={filteredActivity} botState={botState} onShareReward={onShareReward} />
        ) : tab === "positions" ? (
          <PositionsTab items={filteredPositions} walletAddress={walletAddress} onSharePosition={onSharePosition} />
        ) : (
          <OpenOrdersTab
            groups={filteredOpenOrderGroups}
          />
        )}
      </div>
    </SectionPanel>
  );
}
