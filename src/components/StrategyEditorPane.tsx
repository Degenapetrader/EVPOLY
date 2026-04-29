import { useEffect, useMemo, useState, type Dispatch, type SetStateAction } from "react";
import { SectionPanel } from "./SectionPanel";
import {
  parseNonNegative,
  strategyCapValue,
  strategyLabel,
  strategySections,
  strategySizeLabel,
  strategySizeValue,
  strategySummary,
  strategyTimeframeOptions,
  strategySupportsSymbols,
  symbolSetForStrategy,
  updateStrategyCap,
  updateStrategyEnabled,
  updateStrategySettingsSection,
  updateStrategySize,
  updateStrategySymbols,
  type StrategyEditorSection,
  type StrategyKey,
} from "../lib/desktop-config";
import type { BotConfig } from "../lib/tauri-commands";

function renderBooleanChoice(
  value: boolean,
  onChange: (next: boolean) => void,
  disabled = false
) {
  return (
    <div className="flex flex-wrap gap-2">
      <button
        type="button"
        disabled={disabled}
        onClick={() => onChange(true)}
        className={`ui-button ${value ? "ui-button--accent" : ""}`.trim()}
      >
        On
      </button>
      <button
        type="button"
        disabled={disabled}
        onClick={() => onChange(false)}
        className={`ui-button ${!value ? "ui-button--accent" : ""}`.trim()}
      >
        Off
      </button>
    </div>
  );
}

function timeframeLabel(value: string) {
  return value.toUpperCase();
}

function toggleTimeframeSelection(
  list: string[],
  value: string,
  options: readonly string[]
) {
  if (list.includes(value)) {
    return list.length > 1 ? list.filter((item) => item !== value) : list;
  }
  return options.filter((item) => list.includes(item) || item === value);
}

function sizePreview(baseSize: number, symbolMultiplier: number, timeframeMultiplier = 1) {
  return (baseSize * symbolMultiplier * timeframeMultiplier).toFixed(2);
}

export function StrategyEditorPane({
  selectedStrategy,
  config,
  setConfig,
  activeProfileId,
  onSave,
  saveLoading = false,
  dirty = false,
  canSave = false,
  saveMessage = null,
}: {
  selectedStrategy: StrategyKey;
  config: BotConfig;
  setConfig: Dispatch<SetStateAction<BotConfig>>;
  activeProfileId: string | null;
  onSave?: () => void;
  saveLoading?: boolean;
  dirty?: boolean;
  canSave?: boolean;
  saveMessage?: string | null;
}) {
  const [selectedSection, setSelectedSection] = useState<StrategyEditorSection>("general");

  const visibleSections = useMemo(() => strategySections(selectedStrategy), [selectedStrategy]);
  const canEdit = Boolean(activeProfileId);

  useEffect(() => {
    if (!visibleSections.includes(selectedSection)) {
      setSelectedSection(visibleSections[0]);
    }
  }, [selectedSection, visibleSections]);

  const selectedEnabled = config.strategies[selectedStrategy];
  const selectedSizeValue = strategySizeValue(config, selectedStrategy);
  const selectedSizeLabel = strategySizeLabel(selectedStrategy, config);
  const selectedCapValue = strategyCapValue(config, selectedStrategy);
  const allowedSymbols = symbolSetForStrategy(selectedStrategy);
  const activeSymbolCount = allowedSymbols.filter((symbol) => config.symbols.includes(symbol)).length;
  const mmRewardsModeLabel =
    config.strategy_settings.mm_rewards.market_mode === "hybrid" ? "Hybrid" : "Auto";
  const mmSportUsesDepthRatio = config.strategy_settings.mm_sport.quote_size_mode === "depth_ratio";
  const mmSportQuoteModeLabel = mmSportUsesDepthRatio ? "Depth Ratio" : "Quote Multiple";
  const mmSportInventoryExitLabel =
    config.strategy_settings.mm_sport.inventory_exit_mode === "aggressive"
      ? "Aggressive"
      : config.strategy_settings.mm_sport.inventory_exit_mode === "no_exit"
        ? "Feeling Lucky"
        : "Auto";
  const patchPremarket = (patch: Partial<BotConfig["strategy_settings"]["premarket"]>) =>
    setConfig((current) =>
      updateStrategySettingsSection(current, "premarket", {
        ...current.strategy_settings.premarket,
        ...patch,
      })
    );

  const patchEndgame = (patch: Partial<BotConfig["strategy_settings"]["endgame"]>) =>
    setConfig((current) =>
      updateStrategySettingsSection(current, "endgame", {
        ...current.strategy_settings.endgame,
        ...patch,
      })
    );

  const patchEVCurve = (patch: Partial<BotConfig["strategy_settings"]["evcurve"]>) =>
    setConfig((current) =>
      updateStrategySettingsSection(current, "evcurve", {
        ...current.strategy_settings.evcurve,
        ...patch,
      })
    );

  const patchSessionBand = (patch: Partial<BotConfig["strategy_settings"]["session_band"]>) =>
    setConfig((current) =>
      updateStrategySettingsSection(current, "session_band", {
        ...current.strategy_settings.session_band,
        ...patch,
      })
    );

  const patchEVSnipe = (patch: Partial<BotConfig["strategy_settings"]["evsnipe"]>) =>
    setConfig((current) =>
      updateStrategySettingsSection(current, "evsnipe", {
        ...current.strategy_settings.evsnipe,
        ...patch,
      })
    );

  const patchMMRewards = (patch: Partial<BotConfig["strategy_settings"]["mm_rewards"]>) =>
    setConfig((current) =>
      updateStrategySettingsSection(current, "mm_rewards", {
        ...current.strategy_settings.mm_rewards,
        ...patch,
      })
    );

  const patchMMSport = (patch: Partial<BotConfig["strategy_settings"]["mm_sport"]>) =>
    setConfig((current) =>
      updateStrategySettingsSection(current, "mm_sport", {
        ...current.strategy_settings.mm_sport,
        ...patch,
      })
    );

  const updateSymbolMultiplier = (
    key: keyof BotConfig["size_policy"]["symbol_multipliers"],
    value: number
  ) =>
    setConfig((current) => ({
      ...current,
      size_policy: {
        ...current.size_policy,
        symbol_multipliers: {
          ...current.size_policy.symbol_multipliers,
          [key]: value,
        },
      },
    }));

  const updatePremarketTimeframeMultiplier = (
    key: keyof BotConfig["size_policy"]["premarket_timeframe_multipliers"],
    value: number
  ) =>
    setConfig((current) => ({
      ...current,
      size_policy: {
        ...current.size_policy,
        premarket_timeframe_multipliers: {
          ...current.size_policy.premarket_timeframe_multipliers,
          [key]: value,
        },
      },
    }));

  const updateEVCurveTimeframeMultiplier = (
    key: keyof BotConfig["size_policy"]["evcurve_timeframe_multipliers"],
    value: number
  ) =>
    setConfig((current) => ({
      ...current,
      size_policy: {
        ...current.size_policy,
        evcurve_timeframe_multipliers: {
          ...current.size_policy.evcurve_timeframe_multipliers,
          [key]: value,
        },
      },
    }));

  const renderPrimaryMetricTitle = () => selectedSizeLabel;

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
              disabled={!canEdit}
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
            <div className="metric-label">{renderPrimaryMetricTitle()}</div>
            <div className="metric-value">{selectedSizeValue}</div>
            <div className="metric-detail">{selectedSizeLabel}</div>
          </div>
        </div>

        <div className="surface-panel surface-panel--subtle">
          <div className="surface-panel__body">
            <div className="metric-label">
              {strategySupportsSymbols(selectedStrategy) ? "Symbol scope" : "Mode"}
            </div>
            <div className="metric-value">
              {strategySupportsSymbols(selectedStrategy)
                ? `${activeSymbolCount} symbols`
                : selectedStrategy === "mm_rewards"
                  ? mmRewardsModeLabel
                  : mmSportQuoteModeLabel}
            </div>
            <div className="metric-detail">
              {strategySupportsSymbols(selectedStrategy)
                ? `${allowedSymbols.join(", ")}`
                : selectedStrategy === "mm_rewards"
                  ? "Rewards markets can stay fully automatic or blend in your own slugs."
                  : `${mmSportQuoteModeLabel} sizing with ${mmSportInventoryExitLabel} inventory cleanup.`}
            </div>
          </div>
        </div>
      </div>
    </div>
  );

  const renderSymbols = () => (
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
                disabled={!canEdit || locked}
                onClick={() =>
                  setConfig((current) => updateStrategySymbols(current, selectedStrategy, symbol, !active))
                }
                className={`symbol-chip ${active ? "symbol-chip--active" : ""} ${
                  locked ? "symbol-chip--locked" : ""
                }`.trim()}
              >
                {symbol}
              </button>
            );
          })}
        </div>
      </div>
    </div>
  );

  const renderSizeCard = () => (
    <div className="surface-panel">
      <div className="surface-panel__header">
        <div className="surface-panel__copy">
          <h2 className="surface-panel__title">{renderPrimaryMetricTitle()}</h2>
          <p className="surface-panel__subtitle">This is the main sizing control used from the rail.</p>
        </div>
      </div>
      <div className="surface-panel__body">
        <label className="field-label">{selectedSizeLabel}</label>
        <input
          type="number"
          min="0"
          step="0.1"
          value={selectedSizeValue}
          disabled={!canEdit}
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
  );

  const renderCapCard = () => {
    if (selectedCapValue === null) return null;

    return (
      <div className="surface-panel">
        <div className="surface-panel__header">
          <div className="surface-panel__copy">
            <h2 className="surface-panel__title">Strategy Cap</h2>
            <p className="surface-panel__subtitle">
              Limit how much this strategy can deploy at once.
            </p>
          </div>
        </div>
        <div className="surface-panel__body">
          <label className="field-label">Max Exposure (pUSD)</label>
          <input
            type="number"
            min="0"
            step="1"
            value={selectedCapValue}
            disabled={!canEdit}
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
        </div>
      </div>
    );
  };

  const renderTimeframeChoiceCard = (
    title: string,
    subtitle: string,
    options: readonly string[],
    selected: string[],
    onChange: (next: string[]) => void
  ) => (
    <div className="surface-panel xl:col-span-2">
      <div className="surface-panel__header">
        <div className="surface-panel__copy">
          <h2 className="surface-panel__title">{title}</h2>
          <p className="surface-panel__subtitle">{subtitle}</p>
        </div>
      </div>
      <div className="surface-panel__body">
        <div className="flex flex-wrap gap-2">
          {options.map((timeframe) => {
            const active = selected.includes(timeframe);
            return (
              <button
                key={timeframe}
                type="button"
                disabled={!canEdit}
                onClick={() =>
                  onChange(toggleTimeframeSelection(selected, timeframe, options))
                }
                className={`mode-choice ${active ? "mode-choice--active" : ""}`.trim()}
              >
                {timeframeLabel(timeframe)}
              </button>
            );
          })}
        </div>
      </div>
    </div>
  );

  const renderSymbolMultiplierCard = (title = "Symbol multipliers") => (
    <div className="surface-panel">
      <div className="surface-panel__header">
        <div className="surface-panel__copy">
          <h2 className="surface-panel__title">{title}</h2>
          <p className="surface-panel__subtitle">
            Shared symbol weights applied before any strategy-specific multipliers.
          </p>
        </div>
      </div>
      <div className="surface-panel__body">
        <div className="grid gap-4 md:grid-cols-2 xl:grid-cols-4">
          {Object.entries(config.size_policy.symbol_multipliers).map(([key, value]) => (
            <div key={key}>
              <label className="field-label">{key.toUpperCase()}</label>
              <input
                type="number"
                min="0"
                step="0.05"
                value={value}
                disabled={!canEdit}
                onChange={(event) =>
                  updateSymbolMultiplier(
                    key as keyof BotConfig["size_policy"]["symbol_multipliers"],
                    parseNonNegative(event.target.value, value)
                  )
                }
                className="field-input"
              />
            </div>
          ))}
        </div>
      </div>
    </div>
  );

  const renderPremarketTimeframeCard = () => (
    <div className="surface-panel">
      <div className="surface-panel__header">
        <div className="surface-panel__copy">
          <h2 className="surface-panel__title">Premarket Timeframe Multipliers</h2>
          <p className="surface-panel__subtitle">
            Effective size = base size x symbol multiplier x timeframe multiplier.
          </p>
        </div>
      </div>
      <div className="surface-panel__body">
        <div className="grid gap-4 md:grid-cols-2 xl:grid-cols-5">
          {Object.entries(config.size_policy.premarket_timeframe_multipliers).map(([key, value]) => (
            <div key={key}>
              <label className="field-label">{timeframeLabel(key)}</label>
              <input
                type="number"
                min="0"
                step="0.05"
                value={value}
                disabled={!canEdit}
                onChange={(event) =>
                  updatePremarketTimeframeMultiplier(
                    key as keyof BotConfig["size_policy"]["premarket_timeframe_multipliers"],
                    parseNonNegative(event.target.value, value)
                  )
                }
                className="field-input"
              />
            </div>
          ))}
        </div>
      </div>
    </div>
  );

  const renderEVCurveMultiplierCard = () => (
    <div className="space-y-4">
      <div className="surface-panel">
        <div className="surface-panel__header">
          <div className="surface-panel__copy">
            <h2 className="surface-panel__title">EVCurve Timeframe Multipliers</h2>
            <p className="surface-panel__subtitle">
              Effective size = base size x symbol multiplier x timeframe multiplier.
            </p>
          </div>
        </div>
        <div className="surface-panel__body">
          <div className="grid gap-4 md:grid-cols-2 xl:grid-cols-4">
            {Object.entries(config.size_policy.evcurve_timeframe_multipliers).map(([key, value]) => (
              <div key={key}>
                <label className="field-label">{timeframeLabel(key)}</label>
                <input
                  type="number"
                  min="0"
                  step="0.05"
                  value={value}
                  disabled={!canEdit}
                  onChange={(event) =>
                    updateEVCurveTimeframeMultiplier(
                      key as keyof BotConfig["size_policy"]["evcurve_timeframe_multipliers"],
                      parseNonNegative(event.target.value, value)
                    )
                  }
                  className="field-input"
                />
              </div>
            ))}
          </div>
        </div>
      </div>

      <div className="surface-panel surface-panel--subtle">
        <div className="surface-panel__body">
            <div className="metric-label">Sizing Preview</div>
          <div className="metric-value">
            BTC 1H ={" "}
            {sizePreview(
              config.sizing.evcurve,
              config.size_policy.symbol_multipliers.btc,
              config.size_policy.evcurve_timeframe_multipliers.h1
            )}{" "}
            pUSD
          </div>
          <div className="metric-detail">
            SOL 4H ={" "}
            {sizePreview(
              config.sizing.evcurve,
              config.size_policy.symbol_multipliers.sol,
              config.size_policy.evcurve_timeframe_multipliers.h4
            )}{" "}
            pUSD
          </div>
        </div>
      </div>
    </div>
  );

  const renderRisk = () => {
    if (selectedStrategy === "evsnipe" || selectedStrategy === "mm_rewards" || selectedStrategy === "mm_sport") {
      return null;
    }

    return (
      <div className="grid gap-4 xl:grid-cols-2">
        {renderSizeCard()}
        {renderCapCard()}

        {selectedStrategy === "premarket" ? (
          <>
            {renderTimeframeChoiceCard(
              "Timeframes",
              "Choose which Premarket windows are allowed to open new entries.",
              strategyTimeframeOptions("premarket"),
              config.strategy_settings.premarket.timeframes,
              (next) => patchPremarket({ timeframes: next })
            )}
          </>
        ) : null}

        {selectedStrategy === "endgame" ? (
          <>
            {renderTimeframeChoiceCard(
              "Timeframes",
              "Choose which Endgame windows can sweep.",
              strategyTimeframeOptions("endgame"),
              config.strategy_settings.endgame.timeframes,
              (next) => patchEndgame({ timeframes: next })
            )}
            <div className="surface-panel">
              <div className="surface-panel__header">
                <div className="surface-panel__copy">
                  <h2 className="surface-panel__title">Period Controls</h2>
                  <p className="surface-panel__subtitle">
                    Limit how much Endgame can deploy in each event period.
                  </p>
                </div>
              </div>
              <div className="surface-panel__body">
                <label className="field-label">Per-Period Cap (pUSD)</label>
                <input
                  type="number"
                  min="0"
                  step="1"
                  value={config.strategy_settings.endgame.per_period_cap_usd}
                  disabled={!canEdit}
                  onChange={(event) =>
                    patchEndgame({
                      per_period_cap_usd: parseNonNegative(
                        event.target.value,
                        config.strategy_settings.endgame.per_period_cap_usd
                      ),
                    })
                  }
                  className="field-input"
                />
              </div>
            </div>
          </>
        ) : null}

        {selectedStrategy === "evcurve" ? (
          <>
            {renderTimeframeChoiceCard(
              "Timeframes",
              "Enable or disable the EVCurve legs you want active.",
              strategyTimeframeOptions("evcurve"),
              config.strategy_settings.evcurve.timeframes,
              (next) => patchEVCurve({ timeframes: next })
            )}
          </>
        ) : null}

        {selectedStrategy === "session_band" ? (
          <>
            {renderTimeframeChoiceCard(
              "Timeframes",
              "Choose which S-Band windows can trade.",
              strategyTimeframeOptions("session_band"),
              config.strategy_settings.session_band.timeframes,
              (next) => patchSessionBand({ timeframes: next })
            )}
          </>
        ) : null}
      </div>
    );
  };

  const renderAdvanced = () => {
    if (selectedStrategy === "premarket") {
      return (
        <div className="space-y-4">
          {renderSymbolMultiplierCard()}
          {renderPremarketTimeframeCard()}
          <div className="surface-panel">
            <div className="surface-panel__header">
              <div className="surface-panel__copy">
                <h2 className="surface-panel__title">Cancel After Open</h2>
                <p className="surface-panel__subtitle">
                  Cancel timers used after the event opens for each timeframe.
                </p>
              </div>
            </div>
            <div className="surface-panel__body">
              <div className="grid gap-4 md:grid-cols-2 xl:grid-cols-4">
                {([
                  ["m5", "5m"],
                  ["m15", "15m"],
                  ["h1", "1h"],
                  ["h4", "4h"],
                ] as const).map(([key, label]) => (
                  <div key={key}>
                    <label className="field-label">{label} sec</label>
                    <input
                      type="number"
                      min="0"
                      step="1"
                      value={config.strategy_settings.premarket.cancel_after_open_sec[key]}
                      disabled={!canEdit}
                      onChange={(event) =>
                        patchPremarket({
                          cancel_after_open_sec: {
                            ...config.strategy_settings.premarket.cancel_after_open_sec,
                            [key]: parseNonNegative(
                              event.target.value,
                              config.strategy_settings.premarket.cancel_after_open_sec[key]
                            ),
                          },
                        })
                      }
                      className="field-input"
                    />
                  </div>
                ))}
              </div>
            </div>
          </div>
        </div>
      );
    }

    if (selectedStrategy === "endgame") {
      return (
        <div className="space-y-4">
          {renderSymbolMultiplierCard()}
        </div>
      );
    }

    if (selectedStrategy === "evcurve") {
      const evcurve = config.strategy_settings.evcurve;
      return (
        <div className="space-y-4">
          <div className="surface-panel">
            <div className="surface-panel__header">
              <div className="surface-panel__copy">
                <h2 className="surface-panel__title">Sizing Model</h2>
                <p className="surface-panel__subtitle">
                  Daily size is derived from the shared symbol multiplier and the EVCurve timeframe multiplier.
                </p>
              </div>
            </div>
            <div className="surface-panel__body space-y-4">
              <div>
                <label className="field-label">1D Enabled</label>
                {renderBooleanChoice(
                  evcurve.d1_enabled,
                  (next) => patchEVCurve({ d1_enabled: next }),
                  !canEdit
                )}
              </div>
              <div>
                <div className="metric-label">Formula</div>
                <div className="metric-value">Base Size x Symbol Multiplier x Timeframe Multiplier</div>
                <div className="metric-detail">
                  The old D1 cap field is no longer shown here because it did not represent per-trade daily size.
                </div>
              </div>
            </div>
          </div>

          {renderSymbolMultiplierCard()}
          {renderEVCurveMultiplierCard()}
        </div>
      );
    }

    if (selectedStrategy === "session_band") {
      return (
        <div className="space-y-4">
          {renderSymbolMultiplierCard()}
        </div>
      );
    }

    if (selectedStrategy === "evsnipe") {
      const evsnipe = config.strategy_settings.evsnipe;
      const preHitRatio = evsnipe.pre_hit_enabled ? evsnipe.pre_leg_ratio : evsnipe.saved_pre_leg_ratio;
      const strikeWindowPercent = Number((evsnipe.strike_window_pct * 100).toFixed(4));
      return (
        <div className="grid gap-4 xl:grid-cols-2">
          <div className="surface-panel">
            <div className="surface-panel__header">
                <div className="surface-panel__copy">
                  <h2 className="surface-panel__title">Pre-Hit</h2>
                  <p className="surface-panel__subtitle">
                    Control the early entry leg for hit markets before the strike is crossed.
                  </p>
                </div>
            </div>
            <div className="surface-panel__body space-y-4">
              <div>
                <label className="field-label">Pre-Hit Enabled</label>
                {renderBooleanChoice(
                  evsnipe.pre_hit_enabled,
                  (next) =>
                    patchEVSnipe({
                      pre_hit_enabled: next,
                      pre_leg_ratio: next ? preHitRatio : preHitRatio,
                    }),
                  !canEdit
                )}
              </div>
              <div>
                <label className="field-label">Pre-Hit Ratio</label>
                <input
                  type="number"
                  min="0"
                  max="1"
                  step="0.05"
                  value={preHitRatio}
                  disabled={!canEdit}
                  onChange={(event) => {
                    const next = parseNonNegative(event.target.value, preHitRatio);
                    patchEVSnipe({
                      pre_leg_ratio: next,
                      saved_pre_leg_ratio: next,
                    });
                  }}
                  className="field-input"
                />
              </div>
              <div>
                <label className="field-label">Pre-Trigger (Bps)</label>
                <input
                  type="number"
                  min="0"
                  step="0.1"
                  value={evsnipe.pre_trigger_bps}
                  disabled={!canEdit}
                  onChange={(event) =>
                    patchEVSnipe({
                      pre_trigger_bps: parseNonNegative(
                        event.target.value,
                        evsnipe.pre_trigger_bps
                      ),
                    })
                  }
                  className="field-input"
                />
              </div>
            </div>
          </div>

          <div className="surface-panel">
            <div className="surface-panel__header">
                <div className="surface-panel__copy">
                  <h2 className="surface-panel__title">Strike Window</h2>
                  <p className="surface-panel__subtitle">
                    Keep the hit-market watchlist focused on the expiry and strike range you want.
                  </p>
                </div>
            </div>
            <div className="surface-panel__body grid gap-4">
              <div>
                <label className="field-label">Strike Window (%)</label>
                <input
                  type="number"
                  min="0"
                  step="0.1"
                  value={strikeWindowPercent}
                  disabled={!canEdit}
                  onChange={(event) =>
                    patchEVSnipe({
                      strike_window_pct:
                        parseNonNegative(event.target.value, strikeWindowPercent) / 100,
                    })
                  }
                  className="field-input"
                />
                <p className="mt-2 text-sm text-[var(--text-secondary)]">
                  Watches hit markets whose strike is within this percent of the current spot
                  price. Example: 10 means within 10% of spot.
                </p>
              </div>
              <div>
                <label className="field-label">Max Days To Expiry</label>
                <input
                  type="number"
                  min="0"
                  step="1"
                  value={evsnipe.max_days_to_expiry}
                  disabled={!canEdit}
                  onChange={(event) =>
                    patchEVSnipe({
                      max_days_to_expiry: parseNonNegative(
                        event.target.value,
                        evsnipe.max_days_to_expiry
                      ),
                    })
                  }
                  className="field-input"
                />
              </div>
            </div>
          </div>
        </div>
      );
    }

    if (selectedStrategy === "mm_rewards") {
      const mmRewards = config.strategy_settings.mm_rewards;
      return (
        <div className="space-y-4">
          <div className="surface-panel">
            <div className="surface-panel__header">
              <div className="surface-panel__copy">
                <h2 className="surface-panel__title">Min Share Multiple</h2>
                <p className="surface-panel__subtitle">
                  Set how aggressively MM Rewards targets the reward minimum shares.
                </p>
              </div>
            </div>
            <div className="surface-panel__body">
              <label className="field-label">Min Share Multiple</label>
              <input
                type="number"
                min="0"
                step="0.1"
                value={config.mm_tuning.rewards_min_share_multiple}
                disabled={!canEdit}
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

          <div className="grid gap-4 xl:grid-cols-2">
            <div className="surface-panel">
              <div className="surface-panel__header">
                <div className="surface-panel__copy">
                  <h2 className="surface-panel__title">Market Selection</h2>
                  <p className="surface-panel__subtitle">
                    Control how MM Rewards chooses scanner markets and how often it rotates them.
                  </p>
                </div>
              </div>
              <div className="surface-panel__body grid gap-5">
                <div>
                  <label className="field-label">Market Mode</label>
                  <select
                    value={mmRewards.market_mode}
                    disabled={!canEdit}
                    onChange={(event) =>
                      patchMMRewards({
                        market_mode: event.target.value as BotConfig["strategy_settings"]["mm_rewards"]["market_mode"],
                      })
                    }
                    className="field-input"
                  >
                    <option value="auto">Auto</option>
                    <option value="hybrid">Hybrid</option>
                  </select>
                  <p className="mt-2 text-sm text-[var(--text-secondary)]">
                    {mmRewards.market_mode === "hybrid"
                      ? "Hybrid keeps auto discovery and adds the slugs you list below."
                      : "Auto picks markets from the reward scanner for you."}
                  </p>
                </div>
                {mmRewards.market_mode === "hybrid" ? (
                  <div>
                    <label className="field-label">Single Market Slugs</label>
                    <input
                      type="text"
                      value={mmRewards.single_market_slugs}
                      disabled={!canEdit}
                      onChange={(event) =>
                        patchMMRewards({ single_market_slugs: event.target.value })
                      }
                      className="field-input"
                    />
                    <p className="mt-2 text-sm text-[var(--text-secondary)]">
                      Add comma-separated slugs or full market URLs to keep in the hybrid pool.
                    </p>
                  </div>
                ) : null}
                <div className="grid gap-4 md:grid-cols-2">
                  <div>
                    <label className="field-label">Auto Top N</label>
                    <input
                      type="number"
                      min="0"
                      step="1"
                      value={mmRewards.auto_top_n}
                      disabled={!canEdit}
                      onChange={(event) =>
                        patchMMRewards({
                          auto_top_n: parseNonNegative(event.target.value, mmRewards.auto_top_n),
                        })
                      }
                      className="field-input"
                    />
                    <p className="mt-2 text-sm text-[var(--text-secondary)]">
                      Auto mode only considers the top N ranked reward markets each refresh. Lower
                      values stay selective; higher values scan wider.
                    </p>
                  </div>
                  <div>
                    <label className="field-label">Refresh Sec</label>
                    <input
                      type="number"
                      min="0"
                      step="1"
                      value={mmRewards.auto_refresh_sec}
                      disabled={!canEdit}
                      onChange={(event) =>
                        patchMMRewards({
                          auto_refresh_sec: parseNonNegative(
                            event.target.value,
                            mmRewards.auto_refresh_sec
                          ),
                        })
                      }
                      className="field-input"
                    />
                    <p className="mt-2 text-sm text-[var(--text-secondary)]">
                      How often MM Rewards reruns auto market selection and refreshes the pool.
                    </p>
                  </div>
                </div>
              </div>
            </div>

            <div className="surface-panel">
              <div className="surface-panel__header">
                <div className="surface-panel__copy">
                  <h2 className="surface-panel__title">Filters</h2>
                  <p className="surface-panel__subtitle">
                    Keep MM Rewards away from unwanted markets or oversized reward floors.
                  </p>
                </div>
              </div>
              <div className="surface-panel__body grid gap-4">
                <div>
                  <label className="field-label">Blacklist Keywords</label>
                  <input
                    type="text"
                    value={mmRewards.blacklist_keywords}
                    disabled={!canEdit}
                    onChange={(event) => patchMMRewards({ blacklist_keywords: event.target.value })}
                    className="field-input"
                  />
                  <p className="mt-2 text-sm text-[var(--text-secondary)]">
                    Comma-separated words or phrases that should keep MM Rewards away from matching
                    markets.
                  </p>
                </div>
                <div>
                  <label className="field-label">Reward Min Shares Cap</label>
                  <input
                    type="number"
                    min="0"
                    step="1"
                    value={mmRewards.reward_min_shares_cap}
                    disabled={!canEdit}
                    onChange={(event) =>
                      patchMMRewards({
                        reward_min_shares_cap: parseNonNegative(
                          event.target.value,
                          mmRewards.reward_min_shares_cap
                        ),
                      })
                      }
                      className="field-input"
                    />
                  <p className="mt-2 text-sm text-[var(--text-secondary)]">
                    Optional ceiling for oversized reward floors. Set `0` to ignore this filter.
                  </p>
                </div>
              </div>
            </div>
          </div>
        </div>
      );
    }

    if (selectedStrategy === "mm_sport") {
      const mmSport = config.strategy_settings.mm_sport;
      return (
        <div className="space-y-4">
          <div className="surface-panel">
            <div className="surface-panel__header">
              <div className="surface-panel__copy">
                <h2 className="surface-panel__title">Quote Sizing</h2>
                <p className="surface-panel__subtitle">
                  Choose how MM 2.0 sizes quotes and caps pUSD collateral exposure.
                </p>
              </div>
            </div>
              <div className="surface-panel__body grid gap-4">
              <div>
                <label className="field-label">Discovery Route</label>
                <div className="flex flex-wrap gap-2">
                  {[
                    ["sports", "Sports"],
                    ["nonsports", "Non-sports"],
                    ["dual", "Dual"],
                  ].map(([value, label]) => (
                    <button
                      key={value}
                      type="button"
                      disabled={!canEdit}
                      onClick={() =>
                        patchMMSport({
                          discovery_route: value as BotConfig["strategy_settings"]["mm_sport"]["discovery_route"],
                        })
                      }
                      className={`mode-choice ${
                        mmSport.discovery_route === value ? "mode-choice--active" : ""
                      }`.trim()}
                    >
                      {label}
                    </button>
                  ))}
                </div>
                <p className="mt-2 text-sm text-[var(--text-secondary)]">
                  Sports keeps pregame sports checks active. Non-sports uses reward markets without sports metadata. Dual includes both with duplicate markets merged.
                </p>
              </div>
              <div>
                <label className="field-label">Quote Size Mode</label>
                <div className="flex flex-wrap gap-2">
                  {[
                    ["multiple", "Quote Multiple"],
                    ["depth_ratio", "Depth Ratio"],
                  ].map(([value, label]) => (
                    <button
                      key={value}
                      type="button"
                      disabled={!canEdit}
                      onClick={() =>
                        patchMMSport({
                          quote_size_mode: value as BotConfig["strategy_settings"]["mm_sport"]["quote_size_mode"],
                        })
                      }
                      className={`mode-choice ${
                        mmSport.quote_size_mode === value ? "mode-choice--active" : ""
                      }`.trim()}
                    >
                      {label}
                    </button>
                  ))}
                </div>
                <p className="mt-2 text-sm text-[var(--text-secondary)]">
                  {mmSportUsesDepthRatio
                    ? "Depth Ratio sizes quotes from visible book depth and pUSD collateral."
                    : "Quote Multiple sizes from the reward minimum share target."}
                </p>
              </div>
              <div className="grid gap-4 md:grid-cols-2">
                <div>
                  <label className="field-label">Multiple pUSD Cap Mult</label>
                  <input
                    type="number"
                    min="0"
                    max="1"
                    step="0.01"
                    value={mmSport.multiple_collateral_cap_mult}
                    disabled={!canEdit}
                    onChange={(event) =>
                      patchMMSport({
                        multiple_collateral_cap_mult: parseNonNegative(
                          event.target.value,
                          mmSport.multiple_collateral_cap_mult
                        ),
                      })
                    }
                    className="field-input"
                  />
                  <p className="mt-2 text-sm text-[var(--text-secondary)]">
                    Cap for Quote Multiple mode. Default 0.45 means MM 2.0 plans up to 45% of available pUSD collateral.
                  </p>
                </div>
                <div>
                  <label className="field-label">Depth Ratio pUSD Cap Mult</label>
                  <input
                    type="number"
                    min="0"
                    max="1"
                    step="0.01"
                    value={mmSport.depth_ratio_collateral_cap_mult}
                    disabled={!canEdit}
                    onChange={(event) =>
                      patchMMSport({
                        depth_ratio_collateral_cap_mult: parseNonNegative(
                          event.target.value,
                          mmSport.depth_ratio_collateral_cap_mult
                        ),
                      })
                    }
                    className="field-input"
                  />
                  <p className="mt-2 text-sm text-[var(--text-secondary)]">
                    Cap for Depth Ratio mode. Approval and balance status are checked by runtime before live quoting.
                  </p>
                </div>
              </div>
              {mmSportUsesDepthRatio ? (
                <div className="grid gap-4 md:grid-cols-2">
                  <div>
                    <label className="field-label">Max Share Ratio</label>
                    <input
                      type="number"
                      min="0"
                      step="0.01"
                      value={mmSport.max_share_ratio}
                      disabled={!canEdit}
                      onChange={(event) =>
                        patchMMSport({
                          max_share_ratio: parseNonNegative(
                            event.target.value,
                            mmSport.max_share_ratio
                          ),
                        })
                      }
                      className="field-input"
                    />
                    <p className="mt-2 text-sm text-[var(--text-secondary)]">
                      Use a decimal ratio, so 0.05 means 5% of visible bid depth across the passive quote band.
                    </p>
                  </div>
                  <div>
                    <label className="field-label">Min Visible Depth (USD)</label>
                    <input
                      type="number"
                      min="0"
                      step="1"
                      value={mmSport.min_top_depth_usd}
                      disabled={!canEdit}
                      onChange={(event) =>
                        patchMMSport({
                          min_top_depth_usd: parseNonNegative(
                            event.target.value,
                            mmSport.min_top_depth_usd
                          ),
                        })
                      }
                      className="field-input"
                    />
                    <p className="mt-2 text-sm text-[var(--text-secondary)]">
                      Depth Ratio mode stays out when combined visible bid depth across the passive band is too thin.
                    </p>
                  </div>
                </div>
              ) : (
                <div>
                  <label className="field-label">Quote Size Multiplier</label>
                  <input
                    type="number"
                    min="0"
                    step="0.1"
                    value={config.mm_tuning.sport_quote_size_multiplier}
                    disabled={!canEdit}
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
                  <p className="mt-2 text-sm text-[var(--text-secondary)]">
                    1.2 means MM 2.0 quotes at 120% of the reward minimum share size.
                  </p>
                </div>
              )}
            </div>
          </div>

          <div className="grid gap-4 xl:grid-cols-[minmax(0,1fr)_minmax(0,0.9fr)]">
            <div className="surface-panel">
              <div className="surface-panel__header">
                <div className="surface-panel__copy">
                  <h2 className="surface-panel__title">Pacing</h2>
                  <p className="surface-panel__subtitle">
                    Control when MM 2.0 starts cleanup and how long it cools down after a fill.
                  </p>
                </div>
              </div>
              <div className="surface-panel__body grid gap-4">
                <div>
                  <label className="field-label">Min Reward Rate Per Day</label>
                  <input
                    type="number"
                    min="0"
                    step="1"
                    value={mmSport.min_reward_rate_per_day}
                    disabled={!canEdit}
                    onChange={(event) =>
                      patchMMSport({
                        min_reward_rate_per_day: parseNonNegative(
                          event.target.value,
                          mmSport.min_reward_rate_per_day
                        ),
                      })
                      }
                      className="field-input"
                    />
                  <p className="mt-2 text-sm text-[var(--text-secondary)]">
                    Minimum daily liquidity reward rate a market must offer before MM 2.0
                    will quote it.
                  </p>
                </div>
                <div>
                  <label className="field-label">Pause After Fill (Sec)</label>
                  <input
                    type="number"
                    min="0"
                    step="1"
                    value={mmSport.pause_after_fill_sec}
                    disabled={!canEdit}
                    onChange={(event) =>
                      patchMMSport({
                        pause_after_fill_sec: parseNonNegative(
                          event.target.value,
                          mmSport.pause_after_fill_sec
                        ),
                      })
                      }
                      className="field-input"
                    />
                  <p className="mt-2 text-sm text-[var(--text-secondary)]">
                    After a buy fill, `Auto` waits this long before normal cleanup starts. If the
                    market reaches the inventory-exit window first, cleanup can still begin sooner.
                  </p>
                </div>
                <div>
                  <label className="field-label">Inventory Exit Starts (Hours)</label>
                  <input
                    type="number"
                    min="0"
                    step="1"
                    value={mmSport.inventory_exit_start_hours}
                    disabled={!canEdit}
                    onChange={(event) =>
                      patchMMSport({
                        inventory_exit_start_hours: parseNonNegative(
                          event.target.value,
                          mmSport.inventory_exit_start_hours
                        ),
                      })
                    }
                    className="field-input"
                  />
                  <p className="mt-2 text-sm text-[var(--text-secondary)]">
                    MM 2.0 stops opening fresh entry quotes and switches into inventory cleanup
                    this many hours before game start.
                  </p>
                </div>
                <div className="grid gap-4 md:grid-cols-2">
                  <div>
                    <label className="field-label">Quote Expiry Min (Sec)</label>
                    <input
                      type="number"
                      min="0"
                      step="1"
                      value={mmSport.quote_expiry_min_sec}
                      disabled={!canEdit}
                      onChange={(event) =>
                        patchMMSport({
                          quote_expiry_min_sec: parseNonNegative(
                            event.target.value,
                            mmSport.quote_expiry_min_sec
                          ),
                        })
                      }
                      className="field-input"
                    />
                    <p className="mt-2 text-sm text-[var(--text-secondary)]">
                      Shortest quote lifetime MM 2.0 will use before refreshing a resting entry.
                    </p>
                  </div>
                  <div>
                    <label className="field-label">Quote Expiry Max (Sec)</label>
                    <input
                      type="number"
                      min="0"
                      step="1"
                      value={mmSport.quote_expiry_max_sec}
                      disabled={!canEdit}
                      onChange={(event) =>
                        patchMMSport({
                          quote_expiry_max_sec: parseNonNegative(
                            event.target.value,
                            mmSport.quote_expiry_max_sec
                          ),
                        })
                      }
                      className="field-input"
                    />
                    <p className="mt-2 text-sm text-[var(--text-secondary)]">
                      Longest quote lifetime MM 2.0 will allow when market conditions stay calm.
                    </p>
                  </div>
                </div>
              </div>
            </div>

            <div className="surface-panel">
              <div className="surface-panel__header">
                <div className="surface-panel__copy">
                  <h2 className="surface-panel__title">Filters & Inventory Exit</h2>
                  <p className="surface-panel__subtitle">
                    Choose cleanup behavior and keep MM 2.0 away from unwanted markets.
                  </p>
                </div>
              </div>
              <div className="surface-panel__body grid gap-5">
                <div>
                  <label className="field-label">Inventory Exit Mode</label>
                  <div className="flex flex-wrap gap-2">
                    {[
                      ["normal", "Auto"],
                      ["aggressive", "Aggressive"],
                      ["no_exit", "Feeling Lucky"],
                    ].map(([value, label]) => (
                      <button
                        key={value}
                        type="button"
                        disabled={!canEdit}
                        onClick={() =>
                          patchMMSport({
                            inventory_exit_mode:
                              value as BotConfig["strategy_settings"]["mm_sport"]["inventory_exit_mode"],
                          })
                        }
                        className={`mode-choice ${
                          mmSport.inventory_exit_mode === value ? "mode-choice--active" : ""
                        }`.trim()}
                      >
                        {label}
                      </button>
                    ))}
                  </div>
                  <p className="mt-2 text-sm text-[var(--text-secondary)]">
                    {mmSport.inventory_exit_mode === "aggressive"
                      ? "Aggressive starts trying to sell the position immediately after a fill instead of waiting for the normal cooldown."
                      : mmSport.inventory_exit_mode === "no_exit"
                        ? "Feeling Lucky disables forced cleanup exits. Inventory can stay open into live play or all the way to settlement, which is closer to speculative gambling than controlled market making."
                        : "Auto uses the normal cleanup path and best-effort inventory exits, but rare failures can still happen."}
                  </p>
                  {mmSport.inventory_exit_mode === "no_exit" ? (
                    <div className="inline-alert inline-alert--warning">
                      Feeling Lucky is intentionally high risk. Use it only if you are comfortable
                      holding sports inventory without forced cleanup.
                    </div>
                  ) : null}
                </div>
                <div className="grid gap-4 md:grid-cols-2">
                  <div>
                    <label className="field-label">Match Only</label>
                    <div className="flex flex-wrap gap-2">
                      {[
                        { value: false, label: "Off" },
                        { value: true, label: "On" },
                      ].map(({ value, label }) => (
                        <button
                          key={label}
                          type="button"
                          disabled={!canEdit}
                          onClick={() => patchMMSport({ match_only: value })}
                          className={`mode-choice ${
                            mmSport.match_only === value ? "mode-choice--active" : ""
                          }`.trim()}
                        >
                          {label}
                        </button>
                      ))}
                    </div>
                    <p className="mt-2 text-sm text-[var(--text-secondary)]">
                      Off allows sports futures and outrights. On limits discovery to scheduled match-style markets.
                    </p>
                  </div>
                  <div>
                    <label className="field-label">Allowed Sport Leagues</label>
                    <input
                      type="text"
                      value={mmSport.allowed_sport_league_codes}
                      disabled={!canEdit}
                      onChange={(event) =>
                        patchMMSport({ allowed_sport_league_codes: event.target.value })
                      }
                      className="field-input"
                    />
                  </div>
                  <div>
                    <label className="field-label">Blocked Sport Leagues</label>
                    <input
                      type="text"
                      value={mmSport.blocked_sport_league_codes}
                      disabled={!canEdit}
                      onChange={(event) =>
                        patchMMSport({ blocked_sport_league_codes: event.target.value })
                      }
                      className="field-input"
                    />
                  </div>
                </div>
                <div className="grid gap-4 md:grid-cols-2">
                  <div>
                    <label className="field-label">Keyword Allowlist</label>
                    <input
                      type="text"
                      value={mmSport.market_allowlist_keywords}
                      disabled={!canEdit}
                      onChange={(event) =>
                        patchMMSport({ market_allowlist_keywords: event.target.value })
                      }
                      className="field-input"
                    />
                  </div>
                  <div>
                    <label className="field-label">Keyword Blacklist</label>
                    <input
                      type="text"
                      value={mmSport.market_blacklist_keywords}
                      disabled={!canEdit}
                      onChange={(event) =>
                        patchMMSport({ market_blacklist_keywords: event.target.value })
                      }
                      className="field-input"
                    />
                  </div>
                </div>
                <div className="grid gap-4 md:grid-cols-2">
                  <div>
                    <label className="field-label">Blocked Competition Levels</label>
                    <input
                      type="text"
                      value={mmSport.blocked_competition_levels}
                      disabled={!canEdit}
                      onChange={(event) =>
                        patchMMSport({ blocked_competition_levels: event.target.value })
                      }
                      className="field-input"
                    />
                  </div>
                  <div>
                    <label className="field-label">Reward Min Shares Cap</label>
                    <input
                      type="number"
                      min="0"
                      step="1"
                      value={mmSport.reward_min_shares_cap}
                      disabled={!canEdit}
                      onChange={(event) =>
                        patchMMSport({
                          reward_min_shares_cap: parseNonNegative(
                            event.target.value,
                            mmSport.reward_min_shares_cap
                          ),
                        })
                      }
                      className="field-input"
                    />
                  </div>
                </div>
                <p className="text-sm text-[var(--text-secondary)]">
                  League fields accept comma-separated codes. Keyword filters apply to every MM 2.0 route.
                </p>
              </div>
            </div>
          </div>
        </div>
      );
    }

    return null;
  };

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

          <div className="strategy-editor__footer">
            <div className="strategy-editor__footer-copy">
              <div className="strategy-editor__footer-title">
                {dirty ? "Unsaved strategy changes" : "Strategy settings saved"}
              </div>
              <div className="strategy-editor__footer-note">
                Save from here after editing strategy behavior. Runtime controls stay in the top bar.
              </div>
              {saveMessage ? <div className="metric-detail">{saveMessage}</div> : null}
            </div>
            <button
              type="button"
              onClick={onSave}
              disabled={!canSave || saveLoading || !canEdit}
              className="ui-button ui-button--accent"
            >
              {saveLoading ? "Saving..." : dirty ? "Save changes" : "Saved"}
            </button>
          </div>
        </div>
      )}
    </SectionPanel>
  );
}
