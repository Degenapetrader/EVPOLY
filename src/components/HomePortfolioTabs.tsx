import { useMemo, useState } from "react";
import { SectionPanel } from "./SectionPanel";
import { useHomeActivityApi } from "../hooks/useHomeActivityApi";
import { useHomeOpenOrders } from "../hooks/useHomeOpenOrders";
import { useHomePositions } from "../hooks/useHomePositions";
import {
  type HomeApiActivityItem,
  type HomeApiOpenOrderItem,
  type HomeApiPositionItem,
} from "../lib/tauri-commands";
import { formatUsd } from "../lib/desktop-config";

type HomePortfolioTab = "activity" | "positions" | "open-orders";

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

function formatControlValue(value: number): string {
  return new Intl.NumberFormat("en-US", {
    maximumFractionDigits: value % 1 === 0 ? 0 : 2,
  }).format(value);
}

function formatPriceCents(value: number | null | undefined): string {
  if (typeof value !== "number" || !Number.isFinite(value)) return "--";
  const cents = value * 100;
  const digits = Math.abs(cents % 1) > 0 ? 1 : 0;
  return `${cents.toFixed(digits)}¢`;
}

function formatPercent(value: number | null | undefined): string | null {
  if (typeof value !== "number" || !Number.isFinite(value)) return null;
  return `${value >= 0 ? "+" : ""}${value.toFixed(2)}%`;
}

function activityActionClass(action?: string | null): "buy" | "sell" {
  return action?.toLowerCase() === "sold" ? "sell" : "buy";
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

function outcomeTone(outcome?: string | null): "positive" | "negative" | "neutral" {
  return activityOutcomeClass(outcome);
}

function getPositionTraded(position: HomeApiPositionItem): number | null {
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

function getPositionPnl(position: HomeApiPositionItem): number | null {
  if (typeof position.cash_pnl === "number") return position.cash_pnl;
  const value = position.current_value;
  const traded = getPositionTraded(position);
  if (typeof value === "number" && typeof traded === "number") {
    return value - traded;
  }
  return null;
}

function getPositionPercentPnl(position: HomeApiPositionItem): number | null {
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
  thumbnailUrl: string | null;
  orders: HomeApiOpenOrderItem[];
  totalOriginal: number;
  totalMatched: number;
  totalRemaining: number;
  totalNotionalUsd: number;
  nearestExpiration: string | null;
};

function groupOpenOrders(rows: HomeApiOpenOrderItem[]): OpenOrderGroup[] {
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
    <svg
      aria-hidden="true"
      viewBox="0 0 24 24"
      className="home-portfolio-tabs__search-icon"
    >
      <path
        d="M10.5 4.5a6 6 0 1 0 3.79 10.65l4.28 4.27 1.42-1.41-4.27-4.28A6 6 0 0 0 10.5 4.5Zm0 2a4 4 0 1 1 0 8 4 4 0 0 1 0-8Z"
        fill="currentColor"
      />
    </svg>
  );
}

function ActivityTab({ items, botState }: { items: HomeApiActivityItem[]; botState?: string }) {
  if (items.length === 0) {
    return (
      <div className="empty-state">
        {botState === "running"
          ? "No recent wallet activity yet. Filled buys, sells, and redemptions will appear here."
          : "No recent wallet activity yet. Start the bot or finish setup in Settings first."}
      </div>
    );
  }

  return (
    <div className="activity-feed">
      {items.map((item, index) => (
        <div
          key={`${item.timestamp}-${item.transaction_hash ?? index}`}
          className={`activity-feed__row ${
            item.thumbnail_url ? "activity-feed__row--with-thumb" : ""
          }`.trim()}
        >
          <div
            className={`activity-feed__action activity-feed__action--${activityActionClass(
              item.action
            )}`}
          >
            <div className="activity-feed__marker">
              {activityActionClass(item.action) === "sell" ? "-" : "+"}
            </div>
            <div className="activity-feed__action-label">{item.action ?? "Activity"}</div>
          </div>

          {item.thumbnail_url ? (
            <div className="activity-feed__thumb">
              <img
                src={item.thumbnail_url}
                alt=""
                className="activity-feed__thumb-image"
                loading="lazy"
              />
            </div>
          ) : null}

          <div className="activity-feed__content">
            <div className="activity-feed__title">
              {item.market_title || item.title || item.message}
            </div>
            <div className="activity-feed__meta">
              {item.outcome ? (
                <span
                  className={`activity-feed__chip activity-feed__chip--${activityOutcomeClass(
                    item.outcome
                  )}`}
                >
                  {item.outcome}
                </span>
              ) : null}
              {item.quantity !== null && item.quantity !== undefined ? (
                <span>{formatControlValue(item.quantity)} shares</span>
              ) : null}
              {item.price !== null && item.price !== undefined ? (
                <span>@ {formatPriceCents(item.price)}</span>
              ) : null}
            </div>
          </div>

          <div className="activity-feed__aside">
            {item.cashflow_usd !== null && item.cashflow_usd !== undefined ? (
              <div
                className={`activity-feed__value activity-feed__value--${activityValueClass(
                  item.cashflow_usd
                )}`}
              >
                {formatUsd(item.cashflow_usd)}
              </div>
            ) : null}
            <div className="activity-feed__time">{formatRelativeTime(item.timestamp)}</div>
          </div>
        </div>
      ))}
    </div>
  );
}

function PositionsTab({ items }: { items: HomeApiPositionItem[] }) {
  if (items.length === 0) {
    return <div className="empty-state">No open positions found for the active wallet.</div>;
  }

  return (
    <div className="home-ledger">
      <div className="home-ledger__header home-ledger__header--positions">
        <div>Market</div>
        <div>AVG → NOW</div>
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

          return (
            <div key={`${item.condition_id ?? item.token_id ?? "position"}-${index}`} className="home-ledger__row home-ledger__row--positions">
              <div className="home-ledger__market">
                {item.thumbnail_url ? (
                  <div className="home-ledger__thumb">
                    <img src={item.thumbnail_url} alt="" className="home-ledger__thumb-image" loading="lazy" />
                  </div>
                ) : null}
                <div className="home-ledger__market-copy">
                  <div className="home-ledger__market-title">{item.market_title || "Unknown market"}</div>
                  <div className="home-ledger__market-meta">
                    {item.outcome ? (
                      <span className={`activity-feed__chip activity-feed__chip--${outcomeTone(item.outcome)}`}>
                        {item.outcome} {item.avg_price !== null && item.avg_price !== undefined ? formatPriceCents(item.avg_price) : ""}
                      </span>
                    ) : null}
                    {item.size !== null && item.size !== undefined ? (
                      <span>{formatControlValue(item.size)} shares</span>
                    ) : null}
                    {item.redeemable ? <span>Redeemable</span> : null}
                  </div>
                </div>
              </div>
              <div className="home-ledger__metric">{`${formatPriceCents(item.avg_price)} → ${formatPriceCents(item.current_price)}`}</div>
              <div className="home-ledger__metric">{formatUsd(traded)}</div>
              <div className="home-ledger__metric">{formatUsd(item.size)}</div>
              <div className="home-ledger__value">
                <div className="home-ledger__value-primary">{formatUsd(currentValue)}</div>
                {pnl !== null ? (
                  <div className={`home-ledger__value-secondary home-ledger__value-secondary--${valueTone}`}>
                    {formatUsd(pnl)}{pnlPercent !== null ? ` (${formatPercent(pnlPercent)})` : ""}
                  </div>
                ) : null}
              </div>
            </div>
          );
        })}
      </div>
    </div>
  );
}

function OpenOrdersTab({ groups }: { groups: OpenOrderGroup[] }) {
  const [expandedKeys, setExpandedKeys] = useState<Record<string, boolean>>({});

  if (groups.length === 0) {
    return <div className="empty-state">No open orders found for the active wallet.</div>;
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
              <button
                type="button"
                className="home-ledger__row home-ledger__row--orders"
                onClick={() =>
                  setExpandedKeys((current) => ({
                    ...current,
                    [group.key]: !expanded,
                  }))
                }
              >
                <div className="home-ledger__market">
                  {group.thumbnailUrl ? (
                    <div className="home-ledger__thumb">
                      <img src={group.thumbnailUrl} alt="" className="home-ledger__thumb-image" loading="lazy" />
                    </div>
                  ) : null}
                  <div className="home-ledger__market-copy">
                    <div className="home-ledger__market-title">{group.marketTitle}</div>
                    <div className="home-ledger__market-meta">
                      <span>{group.orders.length} orders</span>
                      <span>{expanded ? "Hide details" : "Show details"}</span>
                    </div>
                  </div>
                </div>
                <div className="home-ledger__metric">
                  {formatControlValue(group.totalMatched)} / {formatControlValue(group.totalOriginal)}
                </div>
                <div className="home-ledger__metric">{formatUsd(group.totalNotionalUsd)}</div>
                <div className="home-ledger__metric">{formatRelativeTime(group.nearestExpiration)}</div>
              </button>
              {expanded ? (
                <div className="home-ledger__details">
                  {group.orders.map((order) => (
                    <div key={order.id} className="home-ledger__detail-row">
                      <div className="home-ledger__detail-left">
                        <span className={`activity-feed__chip activity-feed__chip--${activityOutcomeClass(order.outcome)}`}>
                          {order.outcome || order.side || "Order"}
                        </span>
                        <span className="home-ledger__detail-text">
                          {order.side || "Order"} {formatControlValue(order.remaining_size ?? order.original_size ?? 0)} @ {formatPriceCents(order.price)}
                        </span>
                      </div>
                      <div className="home-ledger__detail-right">
                        <span>{formatUsd(order.total_notional_usd)}</span>
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
  botState,
  onOpenLogs,
}: {
  botState?: string;
  onOpenLogs: () => void;
}) {
  const [tab, setTab] = useState<HomePortfolioTab>("activity");
  const [search, setSearch] = useState("");

  const activity = useHomeActivityApi(30, tab === "activity");
  const positions = useHomePositions(80, tab === "positions");
  const openOrders = useHomeOpenOrders(120, tab === "open-orders");

  const query = search.trim().toLowerCase();

  const filteredActivity = useMemo(
    () =>
      [...activity.items]
        .sort((left, right) => right.timestamp.localeCompare(left.timestamp))
        .filter((item) => {
          if (!query) return true;
          return [
            item.market_title,
            item.title,
            item.outcome,
            item.action,
            item.message,
          ]
            .filter(Boolean)
            .some((value) => value!.toLowerCase().includes(query));
        }),
    [activity.items, query]
  );

  const filteredPositions = useMemo(
    () =>
      [...positions.items]
        .filter((item) => {
          if (!query) return true;
          return [
            item.market_title,
            item.market_slug,
            item.event_slug,
            item.outcome,
            item.opposite_outcome,
          ]
            .filter(Boolean)
            .some((value) => value!.toLowerCase().includes(query));
        })
        .sort((left, right) => (right.current_value ?? 0) - (left.current_value ?? 0)),
    [positions.items, query]
  );

  const filteredOpenOrderGroups = useMemo(
    () =>
      groupOpenOrders(
        openOrders.items.filter((item) => {
          if (!query) return true;
          return [item.market_title, item.outcome, item.side, item.id]
            .filter(Boolean)
            .some((value) => value!.toLowerCase().includes(query));
        })
      ),
    [openOrders.items, query]
  );

  const activeState =
    tab === "activity" ? activity : tab === "positions" ? positions : openOrders;
  const activePlaceholder =
    tab === "activity"
      ? "Search activity"
      : tab === "positions"
      ? "Search positions"
      : "Search open orders";

  return (
    <SectionPanel
      title="Portfolio Feed"
      subtitle="Live wallet activity, open positions, and open orders from the active profile."
      actions={
        <button type="button" onClick={onOpenLogs} className="ui-button">
          Open Logs
        </button>
      }
      bodyClassName="home-portfolio-tabs"
    >
      <div className="segmented-control" role="tablist" aria-label="Portfolio tabs">
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

      {activeState.error ? (
        <div className="inline-alert inline-alert--warning">{activeState.error}</div>
      ) : null}

      {activeState.isLoading && activeState.items.length === 0 ? (
        <div className="empty-state">Loading {tab.replace("-", " ")}...</div>
      ) : tab === "activity" ? (
        <ActivityTab items={filteredActivity} botState={botState} />
      ) : tab === "positions" ? (
        <PositionsTab items={filteredPositions} />
      ) : (
        <OpenOrdersTab groups={filteredOpenOrderGroups} />
      )}
    </SectionPanel>
  );
}
