use crate::portfolio_api;
use chrono::Utc;
use serde::Serialize;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use tauri::async_runtime::JoinHandle;
use tokio::sync::watch;

const DEFAULT_INTERVAL_SEC: u64 = 3600;
const DEFAULT_ACTIVITY_LIMIT: usize = 200;

#[derive(Clone)]
pub struct WalletSyncRuntimeConfig {
    pub wallet_address: String,
    pub interval_sec: u64,
    pub activity_limit: usize,
}

impl WalletSyncRuntimeConfig {
    pub fn new(wallet_address: String) -> Self {
        Self {
            wallet_address,
            interval_sec: DEFAULT_INTERVAL_SEC,
            activity_limit: DEFAULT_ACTIVITY_LIMIT,
        }
    }
}

#[derive(Clone, Serialize)]
pub struct WalletSyncStatusSnapshot {
    pub state: String,
    pub managed: bool,
    pub wallet_address: Option<String>,
    pub last_run_at: Option<String>,
    pub last_run_at_ms: Option<i64>,
    pub last_result: Option<String>,
    pub error: Option<String>,
    pub interval_sec: u64,
}

impl Default for WalletSyncStatusSnapshot {
    fn default() -> Self {
        Self {
            state: "inactive".to_string(),
            managed: false,
            wallet_address: None,
            last_run_at: None,
            last_run_at_ms: None,
            last_result: None,
            error: None,
            interval_sec: DEFAULT_INTERVAL_SEC,
        }
    }
}

struct WalletSyncInner {
    status: WalletSyncStatusSnapshot,
    stop_tx: Option<watch::Sender<bool>>,
    task: Option<JoinHandle<()>>,
    in_flight: bool,
}

#[derive(Clone)]
pub struct WalletSyncManager {
    data_dir: PathBuf,
    inner: Arc<Mutex<WalletSyncInner>>,
}

impl WalletSyncManager {
    pub fn new(data_dir: PathBuf) -> Self {
        Self {
            data_dir,
            inner: Arc::new(Mutex::new(WalletSyncInner {
                status: WalletSyncStatusSnapshot::default(),
                stop_tx: None,
                task: None,
                in_flight: false,
            })),
        }
    }

    pub fn start(&self, config: WalletSyncRuntimeConfig) -> Result<(), String> {
        {
            let inner = self.inner.lock().map_err(|e| e.to_string())?;
            let same_wallet = inner
                .status
                .wallet_address
                .as_deref()
                .map(|wallet| wallet.eq_ignore_ascii_case(&config.wallet_address))
                .unwrap_or(false);
            if inner.task.is_some() && same_wallet {
                return Ok(());
            }
        }

        self.stop()?;

        let (stop_tx, mut stop_rx) = watch::channel(false);
        let inner_ref = self.inner.clone();
        let data_dir = self.data_dir.clone();
        let config_for_task = config.clone();

        {
            let mut inner = self.inner.lock().map_err(|e| e.to_string())?;
            inner.status.state = "running".to_string();
            inner.status.managed = true;
            inner.status.wallet_address = Some(config.wallet_address.clone());
            inner.status.interval_sec = config.interval_sec.max(60);
            inner.status.error = None;
            inner.stop_tx = Some(stop_tx);
        }

        let task = tauri::async_runtime::spawn(async move {
            super::append_desktop_debug_line(
                &data_dir,
                "WALLET_SYNC",
                format!(
                    "managed wallet sync started wallet={} interval_sec={}",
                    config_for_task.wallet_address, config_for_task.interval_sec
                )
                .as_str(),
            );

            loop {
                if let Err(err) =
                    run_sync_pass(inner_ref.clone(), data_dir.clone(), config_for_task.clone())
                        .await
                {
                    super::append_desktop_debug_line(
                        &data_dir,
                        "WALLET_SYNC",
                        format!("wallet sync pass failed: {err}").as_str(),
                    );
                }

                tokio::select! {
                    changed = stop_rx.changed() => {
                        if changed.is_ok() && *stop_rx.borrow() {
                            break;
                        }
                    }
                    _ = tokio::time::sleep(std::time::Duration::from_secs(config_for_task.interval_sec.max(60))) => {}
                }
            }

            if let Ok(mut inner) = inner_ref.lock() {
                inner.in_flight = false;
                inner.stop_tx = None;
                inner.task = None;
                inner.status.managed = false;
                inner.status.state = "inactive".to_string();
            }
        });

        let mut inner = self.inner.lock().map_err(|e| e.to_string())?;
        inner.task = Some(task);
        Ok(())
    }

    pub fn stop(&self) -> Result<(), String> {
        let mut inner = self.inner.lock().map_err(|e| e.to_string())?;
        if let Some(tx) = inner.stop_tx.take() {
            let _ = tx.send(true);
        }
        if let Some(task) = inner.task.take() {
            task.abort();
        }
        inner.in_flight = false;
        inner.status.managed = false;
        inner.status.state = "inactive".to_string();
        Ok(())
    }

    pub fn status(&self) -> WalletSyncStatusSnapshot {
        self.inner
            .lock()
            .map(|inner| inner.status.clone())
            .unwrap_or_default()
    }

    pub async fn run_now(
        &self,
        config: WalletSyncRuntimeConfig,
    ) -> Result<WalletSyncStatusSnapshot, String> {
        {
            let inner = self.inner.lock().map_err(|e| e.to_string())?;
            if inner.in_flight {
                return Err("wallet sync is already running".to_string());
            }
        }
        run_sync_pass(self.inner.clone(), self.data_dir.clone(), config).await?;
        Ok(self.status())
    }
}

async fn run_sync_pass(
    inner_ref: Arc<Mutex<WalletSyncInner>>,
    data_dir: PathBuf,
    config: WalletSyncRuntimeConfig,
) -> Result<(), String> {
    {
        let mut inner = inner_ref.lock().map_err(|e| e.to_string())?;
        inner.in_flight = true;
        inner.status.state = if inner.status.managed {
            "running".to_string()
        } else {
            "syncing".to_string()
        };
        inner.status.wallet_address = Some(config.wallet_address.clone());
        inner.status.interval_sec = config.interval_sec.max(60);
        inner.status.error = None;
    }

    let positions = match portfolio_api::fetch_positions(&config.wallet_address, 500).await {
        Ok(value) => value,
        Err(err) => {
            record_sync_failure(inner_ref.clone(), &config, &err)?;
            return Err(err);
        }
    };
    let activity =
        match portfolio_api::fetch_activity(&config.wallet_address, config.activity_limit).await {
            Ok(value) => value,
            Err(err) => {
                record_sync_failure(inner_ref.clone(), &config, &err)?;
                return Err(err);
            }
        };
    let (portfolio_value, source) =
        match portfolio_api::fetch_portfolio_value_with_fallback(&config.wallet_address).await {
            Ok(value) => value,
            Err(err) => {
                record_sync_failure(inner_ref.clone(), &config, &err)?;
                return Err(err);
            }
        };

    let now = Utc::now();
    let summary = format!(
        "wallet sync ok wallet={} positions={} activity={} portfolio_value={:.2} source={}",
        config.wallet_address,
        positions.len(),
        activity.len(),
        portfolio_value,
        source
    );

    {
        let mut inner = inner_ref.lock().map_err(|e| e.to_string())?;
        inner.in_flight = false;
        inner.status.last_run_at = Some(now.to_rfc3339());
        inner.status.last_run_at_ms = Some(now.timestamp_millis());
        inner.status.last_result = Some(summary.clone());
        inner.status.error = None;
        inner.status.state = if inner.status.managed {
            "running".to_string()
        } else {
            "inactive".to_string()
        };
    }

    super::append_desktop_debug_line(&data_dir, "WALLET_SYNC", summary.as_str());
    Ok(())
}

fn record_sync_failure(
    inner_ref: Arc<Mutex<WalletSyncInner>>,
    config: &WalletSyncRuntimeConfig,
    error: &str,
) -> Result<(), String> {
    let now = Utc::now();
    let mut inner = inner_ref.lock().map_err(|e| e.to_string())?;
    inner.in_flight = false;
    inner.status.wallet_address = Some(config.wallet_address.clone());
    inner.status.interval_sec = config.interval_sec.max(60);
    inner.status.last_run_at = Some(now.to_rfc3339());
    inner.status.last_run_at_ms = Some(now.timestamp_millis());
    inner.status.last_result = Some(format!("wallet sync failed: {error}"));
    inner.status.error = Some(error.to_string());
    inner.status.state = if inner.status.managed {
        "degraded".to_string()
    } else {
        "error".to_string()
    };
    Ok(())
}
