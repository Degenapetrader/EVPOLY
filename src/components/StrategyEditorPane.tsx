import { useEffect, useMemo, useState, type Dispatch, type SetStateAction } from "react";
import { SectionPanel } from "./SectionPanel";
import {
  CORE_SYMBOLS,
  EXTRA_SYMBOLS,
  parseNonNegative,
  strategyCapValue,
  strategyLabel,
  strategySections,
  strategySizeLabel,
  strategySizeValue,
  strategySummary,
  updateStrategyCap,
  updateStrategyEnabled,
  updateStrategySize,
  type StrategyEditorSection,
  type StrategyKey,
} from "../lib/desktop-config";
import type { BotConfig } from "../lib/tauri-commands";

function strategyAllowsExtraSymbols(strategy: StrategyKey): boolean {
  return strategy === "endgame" || strategy === "evsnipe";
}

function symbolSetForStrategy(strategy: StrategyKey) {
  return strategyAllowsExtraSymbols(strategy)
    ? [...CORE_SYMBOLS, ...EXTRA_SYMBOLS]
    : [...CORE_SYMBOLS];
}

export function StrategyEditorPane({
  selectedStrategy,
  config,
  setConfig,
  activeProfileId,
}: {
  selectedStrategy: StrategyKey;
  config: BotConfig;
  setConfig: Dispatch<SetStateAction<BotConfig>>;
  activeProfileId: string | null;
}) {
  const [selectedSection, setSelectedSection] = useState<StrategyEditorSection>("general");

  const visibleSections = useMemo(() => strategySections(selectedStrategy), [selectedStrategy]);

  useEffect(() => {
    if (!visibleSections.includes(selectedSection)) {
      setSelectedSection(visibleSections[0]);
    }
  }, [selectedSection, visibleSections]);

  const selectedEnabled = config.strategies[selectedStrategy];
  const selectedSizeValue = strategySizeValue(config, selectedStrategy);
  const selectedSizeLabel = strategySizeLabel(selectedStrategy);
  const selectedCapValue = strategyCapValue(config, selectedStrategy);

  const renderGeneral = () => (
    <div className="space-y-4">
      <div className="surface-panel surface-panel--subtle">
        <div className="surface-panel__body">
          <div className="flex flex-wrap items-start justify-between gap-4">
            <div>
              <div className="text-2xl font-semibold tracking-[-0.05em] text-[var(--text-primary)]">
                {strategyLabel(selectedStrategy)}
              </div>
              <div className="mt-2 max-w-2xl text-sm leading-6 text-[var(--text-secondary)]">
                {strategySummary(selectedStrategy)}
              </div>
            </div>
            <button
              type="button"
              onClick={() =>
                setConfig((current) =>
                  updateStrategyEnabled(current, selectedStrategy, !selectedEnabled)
                )
              }
              className={`ui-button ${selectedEnabled ? "ui-button--accent" : ""}`.trim()}
            >
              {selectedEnabled ? "On" : "Off"}
            </button>
          </div>
        </div>
      </div>

      <div className="grid gap-3 md:grid-cols-3">
        <div className="surface-panel surface-panel--subtle">
          <div className="surface-panel__body">
            <div className="metric-label">Status</div>
            <div className="metric-value">{selectedEnabled ? "Enabled" : "Disabled"}</div>
            <div className="metric-detail">
              {selectedEnabled
                ? "This strategy will participate in live runs."
                : "This strategy is excluded from runtime decisions."}
            </div>
          </div>
        </div>

        <div className="surface-panel surface-panel--subtle">
          <div className="surface-panel__body">
            <div className="metric-label">Primary control</div>
            <div className="metric-value">{selectedSizeValue}</div>
            <div className="metric-detail">{selectedSizeLabel}</div>
          </div>
        </div>

        <div className="surface-panel surface-panel--subtle">
          <div className="surface-panel__body">
            <div className="metric-label">Symbol scope</div>
            <div className="metric-value">
              {symbolSetForStrategy(selectedStrategy).length} symbols
            </div>
            <div className="metric-detail">
              {strategyAllowsExtraSymbols(selectedStrategy)
                ? "Expanded crypto scope is allowed here."
                : "This strategy stays on BTC, ETH, SOL, and XRP."}
            </div>
          </div>
        </div>
      </div>
    </div>
  );

  const renderRisk = () => (
    <div className="grid gap-4 lg:grid-cols-[minmax(0,0.9fr)_minmax(0,1.1fr)]">
      <div className="surface-panel">
        <div className="surface-panel__header">
          <div className="surface-panel__copy">
            <h2 className="surface-panel__title">Sizing</h2>
            <p className="surface-panel__subtitle">
              Adjust the main control value for this strategy.
            </p>
          </div>
        </div>
        <div className="surface-panel__body">
          <label className="field-label">{selectedSizeLabel}</label>
          <input
            type="number"
            min="0"
            step="0.1"
            value={selectedSizeValue}
            onChange={(event) =>
              setConfig((current) =>
                updateStrategySize(
                  current,
                  selectedStrategy,
                  parseNonNegative(event.target.value, selectedSizeValue)
                )
              )
            }
            className="field-input"
          />
        </div>
      </div>

      <div className="surface-panel">
        <div className="surface-panel__header">
          <div className="surface-panel__copy">
            <h2 className="surface-panel__title">Exposure</h2>
            <p className="surface-panel__subtitle">
              Cap the amount this strategy can deploy at once.
            </p>
          </div>
        </div>
        <div className="surface-panel__body">
          {selectedCapValue === null ? (
            <div className="empty-state">
              This strategy does not use a direct max-exposure control in the desktop app.
            </div>
          ) : (
            <>
              <label className="field-label">Max exposure (USD)</label>
              <input
                type="number"
                min="0"
                step="1"
                value={selectedCapValue}
                onChange={(event) =>
                  setConfig((current) =>
                    updateStrategyCap(
                      current,
                      selectedStrategy,
                      parseNonNegative(event.target.value, selectedCapValue)
                    )
                  )
                }
                className="field-input"
              />
            </>
          )}
        </div>
      </div>
    </div>
  );

  const renderSymbols = () => {
    const allowedSymbols = symbolSetForStrategy(selectedStrategy);

    return (
      <div className="surface-panel">
        <div className="surface-panel__header">
          <div className="surface-panel__copy">
            <h2 className="surface-panel__title">Symbols</h2>
            <p className="surface-panel__subtitle">
              Keep the allowed symbol set explicit for the selected strategy.
            </p>
          </div>
        </div>
        <div className="surface-panel__body space-y-4">
          <div className="flex flex-wrap gap-2">
            {allowedSymbols.map((symbol) => {
              const active = config.symbols.includes(symbol);
              const locked = symbol === "BTC";
              return (
                <button
                  key={symbol}
                  type="button"
                  disabled={locked}
                  onClick={() =>
                    setConfig((current) => ({
                      ...current,
                      symbols: active
                        ? current.symbols.filter((item) => item !== symbol)
                        : [...current.symbols, symbol],
                    }))
                  }
                  className={`symbol-chip ${
                    active ? "symbol-chip--active" : ""
                  } ${locked ? "symbol-chip--locked" : ""}`.trim()}
                >
                  {symbol}
                </button>
              );
            })}
          </div>
        </div>
      </div>
    );
  };

  const renderAdvanced = () => (
    <div className="grid gap-4 lg:grid-cols-2">
      {selectedStrategy === "mm_rewards" ? (
        <div className="surface-panel">
          <div className="surface-panel__header">
            <div className="surface-panel__copy">
              <h2 className="surface-panel__title">Rewards threshold</h2>
              <p className="surface-panel__subtitle">
                Raise or lower the minimum share multiple before MM Rewards acts.
              </p>
            </div>
          </div>
          <div className="surface-panel__body">
            <label className="field-label">Min share multiple</label>
            <input
              type="number"
              min="0"
              step="0.1"
              value={config.mm_tuning.rewards_min_share_multiple}
              onChange={(event) =>
                setConfig((current) => ({
                  ...current,
                  mm_tuning: {
                    ...current.mm_tuning,
                    rewards_min_share_multiple: parseNonNegative(
                      event.target.value,
                      current.mm_tuning.rewards_min_share_multiple
                    ),
                  },
                }))
              }
              className="field-input"
            />
          </div>
        </div>
      ) : null}

      {selectedStrategy === "mm_sport" ? (
        <div className="surface-panel">
          <div className="surface-panel__header">
            <div className="surface-panel__copy">
              <h2 className="surface-panel__title">Quote sizing</h2>
              <p className="surface-panel__subtitle">
                Adjust the quote size multiplier used on sports markets.
              </p>
            </div>
          </div>
          <div className="surface-panel__body">
            <label className="field-label">Quote size multiplier</label>
            <input
              type="number"
              min="0"
              step="0.1"
              value={config.mm_tuning.sport_quote_size_multiplier}
              onChange={(event) =>
                setConfig((current) => ({
                  ...current,
                  mm_tuning: {
                    ...current.mm_tuning,
                    sport_quote_size_multiplier: parseNonNegative(
                      event.target.value,
                      current.mm_tuning.sport_quote_size_multiplier
                    ),
                  },
                }))
              }
              className="field-input"
            />
          </div>
        </div>
      ) : null}

      <div className="surface-panel">
        <div className="surface-panel__header">
          <div className="surface-panel__copy">
            <h2 className="surface-panel__title">Advanced note</h2>
            <p className="surface-panel__subtitle">
              Strategy behavior lives here. Wallet setup, tokens, and diagnostics stay in Settings.
            </p>
          </div>
        </div>
        <div className="surface-panel__body">
          <div className="text-sm leading-6 text-[var(--text-secondary)]">
            Keep advanced edits narrow. This view should only hold settings that directly change the
            selected strategy.
          </div>
        </div>
      </div>
    </div>
  );

  return (
    <SectionPanel
      title={strategyLabel(selectedStrategy)}
      subtitle={strategySummary(selectedStrategy)}
      className="surface-panel--subtle"
    >
      {!activeProfileId ? (
        <div className="empty-state">
          Open Settings to create the first profile. Strategy editing becomes active once a profile
          exists.
        </div>
      ) : (
        <div className="space-y-5">
          <div className="flex flex-wrap gap-2">
            {visibleSections.map((section) => (
              <button
                key={section}
                type="button"
                onClick={() => setSelectedSection(section)}
                className={`section-tab ${selectedSection === section ? "section-tab--active" : ""}`.trim()}
              >
                {section}
              </button>
            ))}
          </div>

          {selectedSection === "general" ? renderGeneral() : null}
          {selectedSection === "risk" ? renderRisk() : null}
          {selectedSection === "symbols" ? renderSymbols() : null}
          {selectedSection === "advanced" ? renderAdvanced() : null}
        </div>
      )}
    </SectionPanel>
  );
}
