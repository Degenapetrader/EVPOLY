use std::collections::VecDeque;

use crate::signal_state::{AbnormalFlowEvent, WindowZScore};

#[derive(Debug, Clone)]
pub struct MicroflowConfig {
    pub windows_seconds: Vec<usize>,
    pub impulse_z_threshold: f64,
    pub min_abs_net_notional_1s: f64,
    pub min_samples_per_window: usize,
}

impl Default for MicroflowConfig {
    fn default() -> Self {
        Self {
            windows_seconds: vec![300, 900, 1800, 3600],
            impulse_z_threshold: 4.0,
            min_abs_net_notional_1s: 250_000.0,
            min_samples_per_window: 30,
        }
    }
}

#[derive(Debug, Clone, Copy)]
struct BucketSample {
    net_notional_1s: f64,
}

#[derive(Debug)]
pub struct MicroflowEngine {
    cfg: MicroflowConfig,
    history: VecDeque<BucketSample>,
    current_second: Option<i64>,
    current_net_notional: f64,
    max_window: usize,
}

impl MicroflowEngine {
    pub fn new(cfg: MicroflowConfig) -> Self {
        let max_window = cfg.windows_seconds.iter().copied().max().unwrap_or(3600);
        Self {
            cfg,
            history: VecDeque::with_capacity(max_window + 8),
            current_second: None,
            current_net_notional: 0.0,
            max_window,
        }
    }

    pub fn current_second_net_notional(&self) -> f64 {
        self.current_net_notional
    }

    pub fn ingest_trade(
        &mut self,
        timestamp_ms: i64,
        signed_notional: f64,
        source: &str,
    ) -> Option<AbnormalFlowEvent> {
        let second = timestamp_ms / 1000;
        match self.current_second {
            None => {
                self.current_second = Some(second);
                self.current_net_notional = signed_notional;
                None
            }
            Some(cur) if second < cur => {
                self.current_net_notional += signed_notional;
                None
            }
            Some(cur) if second == cur => {
                self.current_net_notional += signed_notional;
                None
            }
            Some(cur) => {
                let event = self.finalize_current(cur, source);
                let mut missing = cur + 1;
                while missing < second {
                    self.push_sample(0.0);
                    missing += 1;
                }
                self.current_second = Some(second);
                self.current_net_notional = signed_notional;
                event
            }
        }
    }

    pub fn flush_now(&mut self, now_ms: i64, source: &str) -> Option<AbnormalFlowEvent> {
        let second = now_ms / 1000;
        match self.current_second {
            Some(cur) if cur < second => self.finalize_current(cur, source),
            _ => None,
        }
    }

    fn finalize_current(&mut self, second: i64, source: &str) -> Option<AbnormalFlowEvent> {
        let sample = self.current_net_notional;
        let event = self.detect_abnormal_event(second, sample, source);
        self.push_sample(sample);
        self.current_second = Some(second + 1);
        self.current_net_notional = 0.0;
        event
    }

    fn push_sample(&mut self, value: f64) {
        self.history.push_back(BucketSample {
            net_notional_1s: value,
        });
        while self.history.len() > self.max_window {
            self.history.pop_front();
        }
    }

    fn detect_abnormal_event(
        &self,
        second: i64,
        sample: f64,
        source: &str,
    ) -> Option<AbnormalFlowEvent> {
        let mut windows = Vec::new();
        let mut max_abs_z = 0.0_f64;

        for window in &self.cfg.windows_seconds {
            let Some(stats) = self.window_stats(*window) else {
                continue;
            };
            let std = stats.std_dev;
            if std <= f64::EPSILON {
                continue;
            }
            let z = (sample - stats.mean) / std;
            max_abs_z = max_abs_z.max(z.abs());
            windows.push(WindowZScore {
                window_seconds: *window as u32,
                z,
            });
        }

        if windows.is_empty() {
            return None;
        }

        let impulse_z_threshold = self.cfg.impulse_z_threshold.max(0.01);
        if max_abs_z < impulse_z_threshold {
            return None;
        }

        if sample.abs() < self.cfg.min_abs_net_notional_1s {
            return None;
        }

        let side = if sample >= 0.0 { "BUY" } else { "SELL" }.to_string();
        Some(AbnormalFlowEvent {
            source: source.to_string(),
            side,
            timestamp_ms: second * 1000,
            net_notional_1s: sample,
            max_abs_z,
            windows,
        })
    }

    fn window_stats(&self, window: usize) -> Option<WindowStats> {
        if self.history.is_empty() {
            return None;
        }
        let n = window.min(self.history.len());
        if n < self.cfg.min_samples_per_window {
            return None;
        }

        let mut sum = 0.0_f64;
        let mut sum_sq = 0.0_f64;
        for sample in self.history.iter().rev().take(n) {
            let x = sample.net_notional_1s;
            sum += x;
            sum_sq += x * x;
        }
        let n_f = n as f64;
        let mean = sum / n_f;
        let variance = (sum_sq / n_f) - (mean * mean);
        let std_dev = variance.max(0.0).sqrt();
        Some(WindowStats { mean, std_dev })
    }
}

#[derive(Debug, Clone, Copy)]
struct WindowStats {
    mean: f64,
    std_dev: f64,
}
