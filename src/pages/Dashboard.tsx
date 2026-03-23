import { useCallback, useEffect, useMemo, useState } from "react";
import { useNavigate } from "react-router-dom";
import { AppShell } from "../components/AppShell";
import { InfoPill } from "../components/InfoPill";
import { LogsDrawer } from "../components/LogsDrawer";
import { ProfileSwitcher } from "../components/ProfileSwitcher";
import { SectionPanel } from "../components/SectionPanel";
import { StatusBadge } from "../components/StatusBadge";
import { useAppContext } from "../App";
import { useBotStatus } from "../hooks/useBotStatus";
import {
  CORE_SYMBOLS,
  EXTRA_SYMBOLS,
  STRATEGIES,
  DEFAULT_CONFIG,
  formatUsd,
  mergeConfig,
  parseNonNegative,
  strategyCapValue,
  strategyLabel,
  strategySections,
  strategySizeLabel,
  strategySizeValue,
  strategySummary,
  strategySupportsSymbols,
  type DashboardStrategyEditorState,
  type StrategyEditorSection,
  type StrategyKey,
  updateStrategyCap,
  updateStrategyEnabled,
  updateStrategySize,
} from "../lib/desktop-config";
import {
  getActiveProfileId,
  getSavedConfig,
  lockSession,
  saveConfig,
  type BotConfig,
} from "../lib/tauri-commands";

export function Dashboard() {
  const navigate = useNavigate();
  const { activeProfileId, setActiveProfileId, setAuthenticated } = useAppContext();
  const { status } = useBotStatus();
  const [config, setConfig] = useState<BotConfig>(DEFAULT_CONFIG);
  const [selectedStrategy, setSelectedStrategy] = useState<StrategyKey>("endgame");
  const [selectedSection, setSelectedSection] = useState<StrategyEditorSection>("general");
  const [configLoaded, setConfigLoaded] = useState(false);
  const [saveLoading, setSaveLoading] = useState(false);
  const [saveMessage, setSaveMessage] = useState<string | null>(null);
  const [logsOpen, setLogsOpen] = useState(false);
  const [savedSnapshot, setSavedSnapshot] = useState<string>(JSON.stringify(DEFAULT_CONFIG));

  const loadProfileConfig = useCallback(async (profileId: string) => {
    const saved = await getSavedConfig(profileId);
    const merged = mergeConfig(saved);
    setConfig(merged);
    const snapshot = JSON.stringify(merged);
    setSavedSnapshot(snapshot);
    setConfigLoaded(true);
  }, []);

  useEffect(() => {
    getActiveProfileId()
      .then(async (id) => {
        setActiveProfileId(id);
        if (id) {
          await loadProfileConfig(id);
        } else {
          setConfigLoaded(true);
        }
      })
      .catch((err) => {
        setSaveMessage(
          typeof err === "string"
            ? err
            : err instanceof Error
            ? err.message
            : "failed to load the active profile"
        );
        setConfigLoaded(true);
      });
  }, [loadProfileConfig, setActiveProfileId]);

  const handleProfileSwitch = async (profileId: string) => {
    setActiveProfileId(profileId);
    await loadProfileConfig(profileId);
  };

  const handleLock = async () => {
    try {
      await lockSession();
      setActiveProfileId(null);
      setAuthenticated(false);
      navigate("/");
    } catch (err) {
      setSaveMessage(
        typeof err === "string"
          ? err
          : err instanceof Error
          ? err.message
          : "failed to lock the session"
      );
    }
  };

  const dirty = useMemo(() => JSON.stringify(config) !== savedSnapshot, [config, savedSnapshot]);
  const visibleSections = useMemo(() => strategySections(selectedStrategy), [selectedStrategy]);

  useEffect(() => {
    if (!visibleSections.includes(selectedSection)) {
      setSelectedSection(visibleSections[0]);
    }
  }, [selectedSection, visibleSections]);

  const editorState: DashboardStrategyEditorState = {
    selectedStrategy,
    visibleSections,
    dirty,
    hasActiveProfile: Boolean(activeProfileId),
  };

  const selectedEnabled = config.strategies[selectedStrategy];
  const selectedSizeValue = strategySizeValue(config, selectedStrategy);
  const selectedSizeLabel = strategySizeLabel(selectedStrategy);
  const selectedCapValue = strategyCapValue(config, selectedStrategy);

  const handleSave = async () => {
    if (!activeProfileId) {
      setSaveMessage("Open Settings to create and save the first profile.");
      return;
    }
    setSaveLoading(true);
    setSaveMessage(null);
    try {
      await saveConfig(activeProfileId, config);
      const snapshot = JSON.stringify(config);
      setSavedSnapshot(snapshot);
      setSaveMessage("Strategy settings saved. Restart the bot to apply live changes.");
    } catch (err) {
      setSaveMessage(
        typeof err === "string"
          ? err
          : err instanceof Error
          ? err.message
          : "failed to save strategy settings"
      );
    } finally {
      setSaveLoading(false);
    }
  };

  const renderGeneral = () => (
    <div className="space-y-4">
      <div className="rounded-[22px] border border-[var(--border)] bg-[rgba(10,16,24,0.78)] px-5 py-5">
        <div className="flex flex-wrap items-start justify-between gap-4">
          <div>
            <div className="text-xl font-semibold text-[var(--text-primary)]">
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
            className={`ui-button ${
              selectedEnabled ? "ui-button--accent" : ""
            }`.trim()}
          >
            {selectedEnabled ? "Enabled" : "Disabled"}
          </button>
        </div>
      </div>

      <div className="grid gap-3 md:grid-cols-3">
        <div className="surface-panel surface-panel--subtle">
          <div className="surface-panel__body">
            <div className="metric-label">Runtime state</div>
            <div className="metric-value">
              {selectedEnabled ? "Ready to run" : "Excluded from runtime"}
            </div>
          </div>
        </div>
        <div className="surface-panel surface-panel--subtle">
          <div className="surface-panel__body">
            <div className="metric-label">Size setting</div>
            <div className="metric-value">{selectedSizeValue}</div>
            <div className="metric-detail">{selectedSizeLabel}</div>
          </div>
        </div>
        <div className="surface-panel surface-panel--subtle">
          <div className="surface-panel__body">
            <div className="metric-label">Market scope</div>
            <div className="metric-value">
              {strategySupportsSymbols(selectedStrategy)
                ? `${config.symbols.length} symbols`
                : "Managed automatically"}
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
            <p className="surface-panel__subtitle">The main trade size for this strategy.</p>
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
            <p className="surface-panel__subtitle">Keep a hard ceiling on strategy deployment.</p>
          </div>
        </div>
        <div className="surface-panel__body">
          {selectedCapValue === null ? (
            <div className="empty-state">
              This strategy does not use a direct max-exposure control on desktop.
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

  const renderSymbols = () => (
    <div className="surface-panel">
      <div className="surface-panel__header">
        <div className="surface-panel__copy">
          <h2 className="surface-panel__title">Selected symbols</h2>
          <p className="surface-panel__subtitle">
            Desktop keeps the symbol surface explicit, with BTC always pinned on.
          </p>
        </div>
      </div>
      <div className="surface-panel__body space-y-4">
        <div className="flex flex-wrap gap-2">
          {CORE_SYMBOLS.map((symbol) => (
            <button
              key={symbol}
              type="button"
              disabled={symbol === "BTC"}
              onClick={() =>
                setConfig((current) => ({
                  ...current,
                  symbols: current.symbols.includes(symbol)
                    ? current.symbols.filter((item) => item !== symbol)
                    : [...current.symbols, symbol],
                }))
              }
              className={`symbol-chip ${
                config.symbols.includes(symbol) ? "symbol-chip--active" : ""
              } ${symbol === "BTC" ? "symbol-chip--locked" : ""}`.trim()}
            >
              {symbol}
            </button>
          ))}
        </div>
        <div className="flex flex-wrap gap-2">
          {EXTRA_SYMBOLS.map((symbol) => (
            <button
              key={symbol}
              type="button"
              onClick={() =>
                setConfig((current) => ({
                  ...current,
                  symbols: current.symbols.includes(symbol)
                    ? current.symbols.filter((item) => item !== symbol)
                    : [...current.symbols, symbol],
                }))
              }
              className={`symbol-chip ${
                config.symbols.includes(symbol) ? "symbol-chip--active" : ""
              }`.trim()}
            >
              {symbol}
            </button>
          ))}
        </div>
      </div>
    </div>
  );

  const renderAdvanced = () => (
    <div className="grid gap-4 lg:grid-cols-2">
      {selectedStrategy === "mm_rewards" ? (
        <div className="surface-panel">
          <div className="surface-panel__header">
            <div className="surface-panel__copy">
              <h2 className="surface-panel__title">Rewards tuning</h2>
              <p className="surface-panel__subtitle">
                Raise or lower the share multiple required before the MM rewards path acts.
              </p>
            </div>
          </div>
          <div className="surface-panel__body">
            <label className="field-label">Minimum share multiple</label>
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
            <h2 className="surface-panel__title">Operator note</h2>
            <p className="surface-panel__subtitle">
              Advanced controls stay narrow here. Secrets, remote tokens, and raw diagnostics live in Settings.
            </p>
          </div>
        </div>
        <div className="surface-panel__body">
          <div className="text-sm leading-6 text-[var(--text-secondary)]">
            Dashboard owns only strategy behavior. Wallet setup, relayer keys, onboarding, logs, and
            wallet-sync diagnostics stay out of this workspace.
          </div>
        </div>
      </div>
    </div>
  );

  const railItems = [
    { label: "Home", to: "/home" },
    { label: "Dashboard", to: "/dashboard" },
    { label: "Settings", to: "/settings" },
    { label: "Open Logs", onClick: () => setLogsOpen(true) },
  ];

  return (
    <AppShell
      railSubtitle="BY EVPLUS"
      railLogoSrc="/logo.png"
      railLogoAlt="EVPlus"
      railItems={railItems}
      railChildren={
        <SectionPanel title="Selected strategy" subtitle="Tune one strategy at a time so the editor stays readable.">
          <div className="space-y-2">
            <div className="text-base font-semibold text-[var(--text-primary)]">
              {strategyLabel(editorState.selectedStrategy)}
            </div>
            <div className="text-sm text-[var(--text-secondary)]">
              {strategySummary(editorState.selectedStrategy)}
            </div>
          </div>
        </SectionPanel>
      }
      eyebrow="Dashboard"
      title="Advanced Strategy Tuning"
      description="Tune one strategy at a time with real sections for general behavior, risk, symbols, and expert-only controls."
      meta={
        <div className="flex flex-wrap items-center justify-end gap-3">
          <ProfileSwitcher activeProfileId={activeProfileId} onSwitch={(id) => void handleProfileSwitch(id)} />
          <StatusBadge status={status} />
          <button type="button" onClick={() => void handleLock()} className="ui-button">
            Lock
          </button>
          <button
            type="button"
            onClick={handleSave}
            disabled={saveLoading || !configLoaded}
            className="ui-button ui-button--primary"
          >
            {saveLoading ? "Saving..." : dirty ? "Save Dashboard Changes" : "Saved"}
          </button>
        </div>
      }
      banner={
        saveMessage ? (
          <div className="inline-alert inline-alert--warning">{saveMessage}</div>
        ) : null
      }
      contentClassName="page-stack"
    >
      <div className="page-split xl:grid-cols-[18rem_minmax(0,1fr)_18rem]">
        <SectionPanel title="Strategy List" subtitle="Pick the strategy you want to tune.">
          <div className="space-y-2">
            {STRATEGIES.map((strategy) => {
              const selected = strategy.key === selectedStrategy;
              const enabled = config.strategies[strategy.key];
              return (
                <button
                  key={strategy.key}
                  type="button"
                  onClick={() => setSelectedStrategy(strategy.key)}
                  className={`strategy-select ${selected ? "strategy-select--active" : ""}`.trim()}
                >
                  <div className="flex items-center justify-between gap-3">
                    <div className="text-left">
                      <div className="text-sm font-semibold text-[var(--text-primary)]">
                        {strategy.label}
                      </div>
                      <div className="mt-1 text-xs text-[var(--text-secondary)]">
                        {strategy.summary}
                      </div>
                    </div>
                    <InfoPill tone={enabled ? "accent" : "neutral"}>
                      {enabled ? "On" : "Off"}
                    </InfoPill>
                  </div>
                </button>
              );
            })}
          </div>
        </SectionPanel>

        <SectionPanel
          title={`${strategyLabel(selectedStrategy)} Editor`}
          subtitle="The selected strategy owns the full center workspace."
        >
          {!activeProfileId ? (
            <div className="empty-state">
              Open Settings to create the first profile. Dashboard tuning becomes active once a profile exists.
            </div>
          ) : (
            <div className="space-y-5">
              <div className="flex flex-wrap gap-2">
                {visibleSections.map((section) => (
                  <button
                    key={section}
                    type="button"
                    onClick={() => setSelectedSection(section)}
                    className={`section-tab ${
                      selectedSection === section ? "section-tab--active" : ""
                    }`.trim()}
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

        <SectionPanel title="Inspector" subtitle="A compact readout for the strategy you are editing.">
          <div className="space-y-4">
            <div className="rounded-[20px] border border-[var(--border)] bg-[rgba(10,16,24,0.8)] px-4 py-4">
              <div className="metric-label">Selected</div>
              <div className="metric-value">{strategyLabel(selectedStrategy)}</div>
              <div className="metric-detail">{strategySummary(selectedStrategy)}</div>
            </div>

            <div className="rounded-[20px] border border-[var(--border)] bg-[rgba(10,16,24,0.8)] px-4 py-4">
              <div className="metric-label">Current size</div>
              <div className="metric-value">{selectedSizeValue}</div>
              <div className="metric-detail">{selectedSizeLabel}</div>
            </div>

            <div className="rounded-[20px] border border-[var(--border)] bg-[rgba(10,16,24,0.8)] px-4 py-4">
              <div className="metric-label">Exposure</div>
              <div className="metric-value">
                {selectedCapValue === null ? "Managed elsewhere" : formatUsd(selectedCapValue)}
              </div>
            </div>

            <div className="rounded-[20px] border border-[var(--border)] bg-[rgba(10,16,24,0.8)] px-4 py-4 text-sm leading-6 text-[var(--text-secondary)]">
              Save here when you finish tuning. Use Home for runtime overview and Settings for setup, secrets, and diagnostics.
            </div>
          </div>
        </SectionPanel>
      </div>

      <LogsDrawer open={logsOpen} onClose={() => setLogsOpen(false)} />
    </AppShell>
  );
}
