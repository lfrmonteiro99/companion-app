use anyhow::Result;
use chrono::{Local, NaiveDate};
use serde::{Deserialize, Serialize};
use std::path::Path;
use thiserror::Error;

#[derive(Error, Debug)]
pub struct BudgetExceeded {
    pub spent: f64,
    pub limit: f64,
}

impl std::fmt::Display for BudgetExceeded {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Daily budget exceeded: ${:.4} of ${:.4}", self.spent, self.limit)
    }
}

#[derive(Debug, Serialize, Deserialize)]
struct BudgetState {
    spent_usd: f64,
    day: NaiveDate,
}

pub struct BudgetController {
    spent_usd: f64,
    limit_usd: f64,
    day: NaiveDate,
    state_path: std::path::PathBuf,
}

impl BudgetController {
    pub fn new(limit_usd: f64, output_dir: &Path) -> Self {
        let state_path = output_dir.join("budget.json");
        let (spent_usd, day) = Self::load_state(&state_path);
        Self { spent_usd, limit_usd, day, state_path }
    }

    fn load_state(path: &Path) -> (f64, NaiveDate) {
        let today = Local::now().date_naive();
        if let Ok(data) = std::fs::read_to_string(path) {
            if let Ok(state) = serde_json::from_str::<BudgetState>(&data) {
                if state.day == today {
                    return (state.spent_usd, today);
                }
            }
        }
        (0.0, today)
    }

    fn save_state(&self) {
        let state = BudgetState { spent_usd: self.spent_usd, day: self.day };
        let json = match serde_json::to_string(&state) {
            Ok(j) => j,
            Err(e) => {
                tracing::error!("budget: serialize failed: {e}");
                return;
            }
        };
        let tmp = self.state_path.with_extension("json.tmp");
        if let Err(e) = std::fs::write(&tmp, &json) {
            tracing::error!("budget: write {:?} failed: {e}", tmp);
            return;
        }
        if let Err(e) = std::fs::rename(&tmp, &self.state_path) {
            tracing::error!(
                "budget: rename {:?} -> {:?} failed: {e}",
                tmp, self.state_path
            );
        }
    }

    pub fn reset_if_new_day(&mut self) {
        let today = Local::now().date_naive();
        if today != self.day {
            self.spent_usd = 0.0;
            self.day = today;
            self.save_state();
        }
    }

    pub fn try_spend(&mut self, cost: f64) -> Result<(), BudgetExceeded> {
        self.reset_if_new_day();
        if self.spent_usd + cost > self.limit_usd {
            return Err(BudgetExceeded { spent: self.spent_usd, limit: self.limit_usd });
        }
        self.spent_usd += cost;
        self.save_state();
        Ok(())
    }

    pub fn remaining(&self) -> f64 {
        (self.limit_usd - self.spent_usd).max(0.0)
    }

    pub fn spent(&self) -> f64 {
        self.spent_usd
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::env;

    fn temp_dir() -> std::path::PathBuf {
        // Per-test unique path so parallel tests can't stomp on each other's
        // budget.json. Previously this returned the same path per-process
        // which was race-prone across parallel tests in the same module.
        let mut dir = env::temp_dir();
        dir.push(format!(
            "awareness-budget-test-{}-{}",
            std::process::id(),
            chrono::Utc::now().timestamp_nanos_opt().unwrap_or(0)
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn test_try_spend_within_limit() {
        let dir = temp_dir();
        let mut budget = BudgetController::new(1.0, &dir);

        assert_eq!(budget.spent(), 0.0);
        assert!(budget.try_spend(0.50).is_ok());
        assert!((budget.spent() - 0.50).abs() < f64::EPSILON);
        assert!((budget.remaining() - 0.50).abs() < f64::EPSILON);
    }

    #[test]
    fn test_try_spend_over_limit() {
        let dir = temp_dir();
        let mut budget = BudgetController::new(0.10, &dir);

        // First spend is fine
        assert!(budget.try_spend(0.05).is_ok());
        // Second spend exceeds limit
        let result = budget.try_spend(0.10);
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!((err.spent - 0.05).abs() < f64::EPSILON);
        assert!((err.limit - 0.10).abs() < f64::EPSILON);
        // Spent should not have changed after the failed spend
        assert!((budget.spent() - 0.05).abs() < f64::EPSILON);
    }

    fn unique_dir(tag: &str) -> std::path::PathBuf {
        let mut dir = env::temp_dir();
        dir.push(format!(
            "awareness-budget-{}-{}-{}",
            std::process::id(),
            tag,
            chrono::Utc::now().timestamp_nanos_opt().unwrap_or(0)
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn save_state_leaves_no_tmp_file() {
        let dir = unique_dir("no-tmp");
        let mut budget = BudgetController::new(1.0, &dir);
        budget.try_spend(0.10).unwrap();
        let state_path = dir.join("budget.json");
        let tmp_path = dir.join("budget.json.tmp");
        assert!(state_path.exists(), "state file must exist after spend");
        assert!(
            !tmp_path.exists(),
            "temp file must not leak after atomic rename"
        );
    }

    #[test]
    fn save_state_produces_valid_json() {
        let dir = unique_dir("valid-json");
        let mut budget = BudgetController::new(1.0, &dir);
        budget.try_spend(0.25).unwrap();

        let raw = std::fs::read_to_string(dir.join("budget.json")).unwrap();
        // Must be parseable JSON with the expected shape.
        let v: serde_json::Value = serde_json::from_str(&raw)
            .expect("budget.json must be valid JSON");
        assert!(v.get("spent_usd").is_some());
        assert!(v.get("day").is_some());
    }

    #[test]
    fn state_survives_restart() {
        let dir = unique_dir("restart");
        {
            let mut b1 = BudgetController::new(1.0, &dir);
            b1.try_spend(0.30).unwrap();
            assert!((b1.spent() - 0.30).abs() < 1e-9);
        }
        // New controller reading the same dir should pick up the prior state.
        let b2 = BudgetController::new(1.0, &dir);
        assert!(
            (b2.spent() - 0.30).abs() < 1e-9,
            "reloaded budget must match persisted state, got {}",
            b2.spent()
        );
    }

    #[test]
    fn atomic_rename_overwrites_existing_state() {
        let dir = unique_dir("overwrite");
        // Seed the state file with stale content.
        std::fs::write(dir.join("budget.json"), "{\"spent_usd\":0.0,\"day\":\"2000-01-01\"}").unwrap();
        let mut b = BudgetController::new(1.0, &dir);
        // try_spend should trigger reset_if_new_day (different day), then save.
        b.try_spend(0.05).unwrap();
        let raw = std::fs::read_to_string(dir.join("budget.json")).unwrap();
        assert!(
            !raw.contains("2000-01-01"),
            "stale day must have been overwritten, got: {raw}"
        );
    }
}
