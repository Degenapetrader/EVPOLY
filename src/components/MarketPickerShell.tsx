import { useId } from "react";
import { InfoPill } from "./InfoPill";

export type MarketPickerItem = {
  id: string;
  title: string;
  subtitle?: string;
  badge?: string;
};

export function MarketPickerShell({
  searchValue,
  onSearchChange,
  searchReady = false,
  results = [],
  recent = [],
  value,
  onValueChange,
  disabled,
}: {
  searchValue: string;
  onSearchChange: (value: string) => void;
  searchReady?: boolean;
  results?: MarketPickerItem[];
  recent?: MarketPickerItem[];
  value: string;
  onValueChange: (value: string) => void;
  disabled?: boolean;
}) {
  const searchId = useId();
  const fallbackId = useId();

  return (
    <div className="market-picker-shell">
      <div className="market-picker-shell__header">
        <div>
          <div className="market-picker-shell__title">Choose market</div>
          <div className="market-picker-shell__subtitle">
            {searchReady
              ? "Search by market name, slug, or symbol. You can still paste a market ID below if you already have it."
              : "Start the manual service to unlock search, or paste a market ID below."}
          </div>
        </div>
        <InfoPill tone={searchReady ? "success" : "warning"}>
          {searchReady ? "Search ready" : "ID fallback"}
        </InfoPill>
      </div>

      <div className="market-picker-shell__search">
        <label htmlFor={searchId} className="block text-xs text-[var(--text-secondary)]">
          Search market
        </label>
        <input
          id={searchId}
          type="text"
          value={searchValue}
          onChange={(event) => onSearchChange(event.target.value)}
          disabled={disabled || !searchReady}
          placeholder={
            searchReady
              ? "Search by market name, slug, or symbol"
              : "Search unlocks when the manual service is running"
          }
          className="market-picker-shell__input"
        />
      </div>

      {searchReady ? (
        <div className="market-picker-shell__results">
          {results.length > 0 ? (
            results.map((item) => (
              <button
                key={item.id}
                type="button"
                className="market-picker-shell__card"
                onClick={() => {
                  onValueChange(item.id);
                  onSearchChange(item.title);
                }}
              >
                <div>
                  <div className="market-picker-shell__card-title">{item.title}</div>
                  {item.subtitle ? (
                    <div className="market-picker-shell__card-subtitle">{item.subtitle}</div>
                  ) : null}
                </div>
                {item.badge ? <InfoPill tone="accent">{item.badge}</InfoPill> : null}
              </button>
            ))
          ) : searchValue.trim() ? (
            <div className="empty-state">No matching markets yet.</div>
          ) : recent.length > 0 ? (
            <>
              <div className="market-picker-shell__section-label">Recent markets</div>
              {recent.map((item) => (
                <button
                  key={item.id}
                  type="button"
                  className="market-picker-shell__card"
                  onClick={() => {
                    onValueChange(item.id);
                    onSearchChange(item.title);
                  }}
                >
                  <div>
                    <div className="market-picker-shell__card-title">{item.title}</div>
                    {item.subtitle ? (
                      <div className="market-picker-shell__card-subtitle">{item.subtitle}</div>
                    ) : null}
                  </div>
                  {item.badge ? <InfoPill tone="accent">{item.badge}</InfoPill> : null}
                </button>
              ))}
            </>
          ) : (
            <div className="empty-state">Recent manual markets will appear here.</div>
          )}
        </div>
      ) : (
        <div className="market-picker-shell__placeholder">
          <div className="market-picker-shell__section-label">Search preview</div>
          <div className="empty-state">
            Start the manual service to search live markets and pull in recent manual markets here.
          </div>
        </div>
      )}

      <div className="market-picker-shell__fallback">
        <label htmlFor={fallbackId} className="block text-xs text-[var(--text-secondary)]">
          Market ID fallback
        </label>
        <input
          id={fallbackId}
          type="text"
          value={value}
          onChange={(event) => onValueChange(event.target.value)}
          disabled={disabled}
          placeholder="Paste the market ID if you already have it"
          className="market-picker-shell__input"
        />
        <div className="market-picker-shell__hint">
          This stays available even after search is on.
        </div>
      </div>
    </div>
  );
}
