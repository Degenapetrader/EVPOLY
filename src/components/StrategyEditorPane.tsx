import { useEffect, useMemo, useState, type Dispatch, type SetStateAction } from "react";
import { SectionPanel } from "./SectionPanel";
import {
  parseNonNegative,
  type PremarketLadderBucket,
  premarketLadderPricesForMode,
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
  const [premarketLadderBucket, setPremarketLadderBucket] =
    useState<PremarketLadderBucket>("m5");

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
  const premarketLadderModes = {
    m5: config.strategy_settings.premarket.entry_ladder_mode_5m,
    non_m5: config.strategy_settings.premarket.entry_ladder_mode_non_m5,
  } as const;
  const premarketNormalLadderM5 = premarketLadderPricesForMode("normal", "m5");
  const premarketNormalLadderNonM5 = premarketLadderPricesForMode("normal", "non_m5");
  const activePremarketLadderMode = premarketLadderModes[premarketLadderBucket];
  const activePremarketDefaultLadder =
    premarketLadderBucket === "m5" ? premarketNormalLadderM5 : premarketNormalLadderNonM5;
  const activePremarketModeLadder =
    premarketLadderBucket === "m5"
      ? premarketLadderPricesForMode(activePremarketLadderMode, "m5")
      : premarketLadderPricesForMode(activePremarketLadderMode, "non_m5");
  const activePremarketBucketLabel =
    premarketLadderBucket === "m5" ? "5m" : "15m / 1h / 4h";

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
          <label className="field-label">Max Exposure (USD)</label>
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
            USD
          </div>
          <div className="metric-detail">
            SOL 4H ={" "}
            {sizePreview(
              config.sizing.evcurve,
              config.size_policy.symbol_multipliers.sol,
              config.size_policy.evcurve_timeframe_multipliers.h4
            )}{" "}
            USD
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
            <div className="surface-panel xl:col-span-2">
              <div className="surface-panel__header">
                <div className="surface-panel__copy">
                  <h2 className="surface-panel__title">Ladder Mode</h2>
                  <p className="surface-panel__subtitle">
                    Set the entry ladder separately for 5m and slower Premarket buckets.
                  </p>
                </div>
              </div>
              <div className="surface-panel__body space-y-4">
                <div className="inline-flex w-full flex-wrap rounded-[1rem] border border-[var(--border)] bg-[rgba(8,12,20,0.82)] p-1">
                  {([
                    ["m5", "5m", "Fast bucket"],
                    ["non_m5", "15m / 1h / 4h", "Slower buckets"],
                  ] as const).map(([bucket, label, detail]) => (
                    <button
                      key={bucket}
                      type="button"
                      disabled={!canEdit}
                      onClick={() => setPremarketLadderBucket(bucket)}
                      className={`flex min-w-[14rem] flex-1 items-center justify-center gap-2 rounded-[0.85rem] px-4 py-3 text-sm font-semibold transition ${
                        premarketLadderBucket === bucket
                          ? "bg-[linear-gradient(180deg,#6bb7ff_0%,#4f9dff_100%)] text-[#07111c]"
                          : "text-[var(--text-secondary)]"
                      }`.trim()}
                    >
                      <span>{label}</span>
                      <span
                        className={`text-xs font-medium ${
                          premarketLadderBucket === bucket
                            ? "text-[rgba(7,17,28,0.78)]"
                            : "text-[var(--text-secondary)]"
                        }`.trim()}
                      >
                        {detail}
                      </span>
                    </button>
                  ))}
                </div>

                <div className="rounded-[1.2rem] border border-[var(--border)] bg-[rgba(10,16,24,0.78)] p-5">
                  <div className="flex flex-wrap items-start justify-between gap-3">
                    <div>
                      <div className="text-xl font-semibold tracking-[-0.04em] text-[var(--text-primary)]">
                        {activePremarketBucketLabel} Ladder
                      </div>
                      <p className="mt-2 max-w-3xl text-sm leading-6 text-[var(--text-secondary)]">
                        Pick how aggressive this timeframe bucket should bid before market open.
                        Only the buy prices move. The budget split stays fixed at 23%, 23%, 17%,
                        14%, 12%, and 11%.
                      </p>
                    </div>
                    <div className="rounded-full border border-[var(--border)] bg-[rgba(8,12,20,0.85)] px-3 py-1 text-xs font-semibold uppercase tracking-[0.22em] text-[var(--text-secondary)]">
                      {activePremarketBucketLabel}
                    </div>
                  </div>

                  <div className="mt-4 grid gap-2 md:grid-cols-3">
                    {([
                      ["normal", "Normal", "Current"],
                      ["safe", "Safe", "10% lower"],
                      ["aggressive", "Aggressive", "10% higher"],
                    ] as const).map(([value, label, detail]) => (
                      <button
                        key={`${premarketLadderBucket}-${value}`}
                        type="button"
                        disabled={!canEdit}
                        onClick={() =>
                          patchPremarket(
                            premarketLadderBucket === "m5"
                              ? { entry_ladder_mode_5m: value }
                              : { entry_ladder_mode_non_m5: value }
                          )
                        }
                        className={`mode-choice flex items-center justify-center gap-2 ${
                          activePremarketLadderMode === value ? "mode-choice--active" : ""
                        }`.trim()}
                      >
                        <span>{label}</span>
                        <span className="text-sm font-medium text-[var(--text-secondary)]">
                          {detail}
                        </span>
                      </button>
                    ))}
                  </div>

                  <div className="mt-4 rounded-[1rem] border border-[var(--border)] bg-[rgba(8,12,20,0.9)] p-4">
                    <div className="flex flex-wrap items-center justify-between gap-3">
                      <div className="text-base font-semibold text-[var(--text-primary)]">
                        {activePremarketLadderMode === "safe"
                          ? "Safe Bids 10% Lower"
                          : activePremarketLadderMode === "aggressive"
                            ? "Aggressive Bids 10% Higher"
                            : "Normal Uses The Default Ladder"}
                      </div>
                      <div className="text-xs font-medium uppercase tracking-[0.18em] text-[var(--text-secondary)]">
                        Mode Preview
                      </div>
                    </div>
                    <div className="mt-4 grid gap-3 lg:grid-cols-3">
                      <div className="rounded-[0.9rem] border border-[var(--border)] bg-[rgba(10,16,24,0.75)] p-3">
                        <div className="metric-label">Default Ladder</div>
                        <div className="mt-2 text-sm leading-6 text-[var(--text-secondary)]">
                          {activePremarketDefaultLadder.map((price) => price.toFixed(2)).join(", ")}
                        </div>
                      </div>
                      <div className="rounded-[0.9rem] border border-[var(--border)] bg-[rgba(10,16,24,0.75)] p-3">
                        <div className="metric-label">Selected Ladder</div>
                        <div className="mt-2 text-sm leading-6 text-[var(--text-secondary)]">
                          {activePremarketModeLadder.map((price) => price.toFixed(2)).join(", ")}
                        </div>
                      </div>
                      <div className="rounded-[0.9rem] border border-[var(--border)] bg-[rgba(10,16,24,0.75)] p-3">
                        <div className="metric-label">Change Preview</div>
                        <div className="mt-2 text-sm leading-6 text-[var(--text-secondary)]">
                          {activePremarketDefaultLadder[0].toFixed(2)} to{" "}
                          {activePremarketModeLadder[0].toFixed(2)},{" "}
                          {activePremarketDefaultLadder[1].toFixed(2)} to{" "}
                          {activePremarketModeLadder[1].toFixed(2)}
                        </div>
                      </div>
                    </div>
                  </div>
                </div>
              </div>
            </div>
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
                <label className="field-label">Per-Period Cap (USD)</label>
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
            <div className="surface-panel">
              <div className="surface-panel__header">
                <div className="surface-panel__copy">
                  <h2 className="surface-panel__title">Entry Filters</h2>
                  <p className="surface-panel__subtitle">
                    Tune the flip probability ceiling and the minimum buy price gate.
                  </p>
                </div>
              </div>
              <div className="surface-panel__body grid gap-4 md:grid-cols-2">
                <div>
                  <label className="field-label">Max Flip Probability</label>
                  <input
                    type="number"
                    min="0"
                    max="1"
                    step="0.01"
                    value={config.strategy_settings.evcurve.max_flip_prob}
                    disabled={!canEdit}
                    onChange={(event) =>
                      patchEVCurve({
                        max_flip_prob: parseNonNegative(
                          event.target.value,
                          config.strategy_settings.evcurve.max_flip_prob
                        ),
                      })
                    }
                    className="field-input"
                  />
                </div>
                <div>
                  <label className="field-label">Min Buy Price</label>
                  <input
                    type="number"
                    min="0"
                    max="1"
                    step="0.01"
                    value={config.strategy_settings.evcurve.min_buy_price}
                    disabled={!canEdit}
                    onChange={(event) =>
                      patchEVCurve({
                        min_buy_price: parseNonNegative(
                          event.target.value,
                          config.strategy_settings.evcurve.min_buy_price
                        ),
                      })
                    }
                    className="field-input"
                  />
                </div>
              </div>
            </div>
          </>
        ) : null}

        {selectedStrategy === "session_band" ? (
          <>
            {renderTimeframeChoiceCard(
              "Timeframes",
              "Choose which SessionBand windows can trade.",
              strategyTimeframeOptions("session_band"),
              config.strategy_settings.session_band.timeframes,
              (next) => patchSessionBand({ timeframes: next })
            )}
            <div className="surface-panel">
              <div className="surface-panel__header">
                <div className="surface-panel__copy">
                  <h2 className="surface-panel__title">Band Threshold</h2>
                  <p className="surface-panel__subtitle">
                    Control how far the lead price must flip before SessionBand acts.
                  </p>
                </div>
              </div>
              <div className="surface-panel__body">
                <label className="field-label">Flip Threshold %</label>
                <input
                  type="number"
                  min="0"
                  step="0.1"
                  value={config.strategy_settings.session_band.flip_threshold_pct}
                  disabled={!canEdit}
                  onChange={(event) =>
                    patchSessionBand({
                      flip_threshold_pct: parseNonNegative(
                        event.target.value,
                        config.strategy_settings.session_band.flip_threshold_pct
                      ),
                    })
                  }
                  className="field-input"
                />
              </div>
            </div>
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
      const endgame = config.strategy_settings.endgame;
      const total = endgame.tick0_multiplier + endgame.tick1_multiplier + endgame.tick2_multiplier;
      return (
        <div className="space-y-4">
          {renderSymbolMultiplierCard()}
          <div className="surface-panel">
            <div className="surface-panel__header">
              <div className="surface-panel__copy">
                <h2 className="surface-panel__title">Tick Split</h2>
                <p className="surface-panel__subtitle">
                  Split the base size across Tick 0, Tick 1, and Tick 2.
                </p>
              </div>
            </div>
            <div className="surface-panel__body space-y-4">
              {([
                ["tick0_multiplier", "Tick 0"],
                ["tick1_multiplier", "Tick 1"],
                ["tick2_multiplier", "Tick 2"],
              ] as const).map(([key, label]) => (
                <div key={key}>
                  <label className="field-label">{label}</label>
                  <input
                    type="number"
                    min="0"
                    step="0.05"
                    value={endgame[key]}
                    disabled={!canEdit}
                    onChange={(event) =>
                      patchEndgame({
                        [key]: parseNonNegative(event.target.value, endgame[key]),
                      } as Partial<BotConfig["strategy_settings"]["endgame"]>)
                    }
                    className="field-input"
                  />
                </div>
              ))}
              <div className="metric-detail">
                Current total: {(total * 100).toFixed(0)}% | Rail tooltip:{" "}
                {`${Math.round(endgame.tick0_multiplier * 100)} / ${Math.round(
                  endgame.tick1_multiplier * 100
                )} / ${Math.round(endgame.tick2_multiplier * 100)}`}
              </div>
            </div>
          </div>
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
      const sessionBand = config.strategy_settings.session_band;
      return (
        <div className="space-y-4">
          <div className="surface-panel">
            <div className="surface-panel__header">
              <div className="surface-panel__copy">
                <h2 className="surface-panel__title">Tau Windows</h2>
                <p className="surface-panel__subtitle">Control which late windows can trigger and how heavily they size.</p>
              </div>
            </div>
            <div className="surface-panel__body space-y-4">
              <div className="grid gap-4 md:grid-cols-2">
                <div>
                  <label className="field-label">T-2</label>
                  {renderBooleanChoice(
                    sessionBand.tau2_enabled,
                    (next) => patchSessionBand({ tau2_enabled: next }),
                    !canEdit
                  )}
                </div>
                <div>
                  <label className="field-label">T-2 Multiplier</label>
                  <input
                    type="number"
                    min="0"
                    step="0.05"
                    value={sessionBand.tau2_multiplier}
                    disabled={!canEdit}
                    onChange={(event) =>
                      patchSessionBand({
                        tau2_multiplier: parseNonNegative(
                          event.target.value,
                          sessionBand.tau2_multiplier
                        ),
                      })
                    }
                    className="field-input"
                  />
                </div>
              </div>
              <div className="grid gap-4 md:grid-cols-2">
                <div>
                  <label className="field-label">T-1</label>
                  {renderBooleanChoice(
                    sessionBand.tau1_enabled,
                    (next) => patchSessionBand({ tau1_enabled: next }),
                    !canEdit
                  )}
                </div>
                <div>
                  <label className="field-label">T-1 Multiplier</label>
                  <input
                    type="number"
                    min="0"
                    step="0.05"
                    value={sessionBand.tau1_multiplier}
                    disabled={!canEdit}
                    onChange={(event) =>
                      patchSessionBand({
                        tau1_multiplier: parseNonNegative(
                          event.target.value,
                          sessionBand.tau1_multiplier
                        ),
                      })
                    }
                    className="field-input"
                  />
                </div>
              </div>
            </div>
          </div>

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
                    Choose how MM Rewards picks markets and how often it refreshes.
                  </p>
                </div>
              </div>
              <div className="surface-panel__body grid gap-4">
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
                <div className="grid gap-4 md:grid-cols-3">
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
                  </div>
                  <div>
                    <label className="field-label">Rank Budget (USD)</label>
                    <input
                      type="number"
                      min="0"
                      step="1"
                      value={mmRewards.auto_rank_budget_usd}
                      disabled={!canEdit}
                      onChange={(event) =>
                        patchMMRewards({
                          auto_rank_budget_usd: parseNonNegative(
                            event.target.value,
                            mmRewards.auto_rank_budget_usd
                          ),
                        })
                      }
                      className="field-input"
                    />
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
                  Choose whether MM Sport sizes from reward multiples or visible book depth.
                </p>
              </div>
            </div>
            <div className="surface-panel__body grid gap-4">
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
                    ? "Depth Ratio sizes quotes from visible book depth and available buying power."
                    : "Quote Multiple sizes from the reward minimum share target."}
                </p>
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
                      Use a decimal ratio, so 0.05 means 5% of visible top-of-book depth.
                    </p>
                  </div>
                  <div>
                    <label className="field-label">Min Top Depth (USD)</label>
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
                      Depth Ratio mode stays out when the visible top depth is too thin.
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
                    1.2 means MM Sport quotes at 120% of the reward minimum share size.
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
                    Control quote pacing and near-expiry exit timing.
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
                </div>
                <div>
                  <label className="field-label">Near-Expiry Exit Window (Sec)</label>
                  <input
                    type="number"
                    min="0"
                    step="1"
                    value={mmSport.near_expiry_exit_window_sec}
                    disabled={!canEdit}
                    onChange={(event) =>
                      patchMMSport({
                        near_expiry_exit_window_sec: parseNonNegative(
                          event.target.value,
                          mmSport.near_expiry_exit_window_sec
                        ),
                      })
                    }
                    className="field-input"
                  />
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
                  </div>
                </div>
              </div>
            </div>

            <div className="surface-panel">
              <div className="surface-panel__header">
                <div className="surface-panel__copy">
                  <h2 className="surface-panel__title">Inventory Exit</h2>
                  <p className="surface-panel__subtitle">
                    Choose how MM Sport exits inventory when it needs to clean up.
                  </p>
                </div>
              </div>
              <div className="surface-panel__body grid gap-4">
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
                            inventory_exit_mode: value as BotConfig["strategy_settings"]["mm_sport"]["inventory_exit_mode"],
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
                      ? "Aggressive leans harder on the bid to exit sooner."
                      : mmSport.inventory_exit_mode === "no_exit"
                        ? "Feeling Lucky skips forced cleanup exits and lets inventory ride."
                        : "Auto uses the normal cleanup path when MM Sport needs to exit."}
                  </p>
                </div>
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
