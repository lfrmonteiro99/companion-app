use anyhow::Result;
use chrono::{Local, NaiveDate};
use serde::{Deserialize, Serialize};
use std::path::Path;
use thiserror::Error;

/// Number of ledger mutations between fsync'd writes. Any pending change is
/// flushed on `flush()` (wired to the main loop's shutdown path) and on
/// day-rollover, so the worst-case data loss on crash is `BATCH_WRITE_N - 1`
/// spends. Chosen to trade ~10x fewer writes for at most a few cents of
/// drift if the process is SIGKILL'd mid-session.
const BATCH_WRITE_N: u32 = 10;

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

/// Proof-of-reservation returned by `try_reserve`. The holder must either
/// `commit` (reconciling estimate vs. real cost) or `refund` (API call
/// failed). `must_use` keeps callers honest — a leaked Reservation means
/// the budget is permanently over-counted by `amount` until process exit.
#[must_use = "budget reservations must be committed or refunded"]
pub struct Reservation {
    amount: f64,
}

impl Reservation {
    pub fn amount(&self) -> f64 {
        self.amount
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
    writes_since_flush: u32,
}

impl BudgetController {
    pub fn new(limit_usd: f64, output_dir: &Path) -> Self {
        let state_path = output_dir.join("budget.json");
        let (spent_usd, day) = Self::load_state(&state_path);
        Self { spent_usd, limit_usd, day, state_path, writes_since_flush: 0 }
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

    /// Record a mutation and flush to disk every BATCH_WRITE_N mutations.
    /// Callers must `flush()` explicitly on shutdown to drain the tail.
    fn mark_dirty(&mut self) {
        self.writes_since_flush += 1;
        if self.writes_since_flush >= BATCH_WRITE_N {
            self.save_state();
            self.writes_since_flush = 0;
        }
    }

    /// Force-write the current state to disk. Idempotent; safe to call even
    /// when there are no pending mutations.
    pub fn flush(&mut self) {
        self.save_state();
        self.writes_since_flush = 0;
    }

    pub fn reset_if_new_day(&mut self) {
        let today = Local::now().date_naive();
        if today != self.day {
            self.spent_usd = 0.0;
            self.day = today;
            // Day rollover is significant — always flush immediately so a
            // later crash can't resurrect yesterday's balance.
            self.save_state();
            self.writes_since_flush = 0;
        }
    }

    /// Reserve an upper-bound cost BEFORE the API call. Returns a Reservation
    /// the caller must later `commit` or `refund`. This is the race-safe
    /// entry point: two concurrent callers cannot both pass the limit check
    /// because the first one's amount is already counted toward `spent_usd`
    /// when the second one evaluates.
    pub fn try_reserve(&mut self, estimate: f64) -> Result<Reservation, BudgetExceeded> {
        self.reset_if_new_day();
        if self.spent_usd + estimate > self.limit_usd {
            return Err(BudgetExceeded { spent: self.spent_usd, limit: self.limit_usd });
        }
        self.spent_usd += estimate;
        self.mark_dirty();
        Ok(Reservation { amount: estimate })
    }

    /// Reconcile a reservation with the real cost (delta = actual - estimate,
    /// can be positive or negative). Always succeeds — we already charged
    /// the estimate, so going slightly over is preferable to under-counting.
    pub fn commit(&mut self, reservation: Reservation, actual_cost: f64) {
        let delta = actual_cost - reservation.amount;
        self.spent_usd += delta;
        // Clamp to 0 to defend against floating-point drift making spent
        // negative if actual is very close to 0 and estimate was generous.
        if self.spent_usd < 0.0 {
            self.spent_usd = 0.0;
        }
        self.mark_dirty();
    }

    /// Release a reservation whose API call never happened (error, timeout,
    /// cancellation). Restores the reserved amount to the pool.
    pub fn refund(&mut self, reservation: Reservation) {
        self.spent_usd -= reservation.amount;
        if self.spent_usd < 0.0 {
            self.spent_usd = 0.0;
        }
        self.mark_dirty();
    }

    /// Reserve-then-commit in one call. Exists for tests and callers that
    /// know the exact cost up front. Prefer `try_reserve` + `commit` when
    /// the real cost is only known after an async call.
    pub fn try_spend(&mut self, cost: f64) -> Result<(), BudgetExceeded> {
        let r = self.try_reserve(cost)?;
        self.commit(r, cost);
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
        // Writes are batched; force one so the file exists.
        budget.flush();
        let state_path = dir.join("budget.json");
        let tmp_path = dir.join("budget.json.tmp");
        assert!(state_path.exists(), "state file must exist after flush");
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
        budget.flush();

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
            // Flush before drop so the next controller picks up the state.
            b1.flush();
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
    fn two_reservations_cannot_both_overshoot() {
        // The race we're defending against: two callers check remaining()
        // concurrently, both see budget available, both call try_spend.
        // With reserve+commit, the second reserve must fail.
        let dir = unique_dir("double-reserve");
        let mut b = BudgetController::new(0.10, &dir);
        let r1 = b.try_reserve(0.08).expect("first reservation fits");
        let r2 = b.try_reserve(0.08);
        assert!(
            r2.is_err(),
            "second reservation must be rejected once the first has claimed the budget"
        );
        // Commit the first at actual cost; budget consistent afterwards.
        b.commit(r1, 0.07);
        assert!((b.spent() - 0.07).abs() < 1e-9, "spent must reflect actual cost, got {}", b.spent());
    }

    #[test]
    fn refund_releases_the_reserved_amount() {
        let dir = unique_dir("refund");
        let mut b = BudgetController::new(1.0, &dir);
        let r = b.try_reserve(0.40).unwrap();
        assert!((b.spent() - 0.40).abs() < 1e-9);
        b.refund(r);
        assert!(
            b.spent().abs() < 1e-9,
            "refund must return spent to zero, got {}",
            b.spent()
        );
    }

    #[test]
    fn commit_reconciles_overestimate_down() {
        let dir = unique_dir("reconcile-down");
        let mut b = BudgetController::new(1.0, &dir);
        let r = b.try_reserve(0.02).unwrap();
        // Real API cost turned out to be much less.
        b.commit(r, 0.0003);
        assert!((b.spent() - 0.0003).abs() < 1e-9, "got {}", b.spent());
    }

    #[test]
    fn commit_reconciles_underestimate_up() {
        let dir = unique_dir("reconcile-up");
        let mut b = BudgetController::new(1.0, &dir);
        let r = b.try_reserve(0.005).unwrap();
        // Real cost was slightly higher than estimate.
        b.commit(r, 0.01);
        assert!((b.spent() - 0.01).abs() < 1e-9, "got {}", b.spent());
    }

    #[test]
    fn batched_writes_flush_on_explicit_flush() {
        let dir = unique_dir("batched-flush");
        let mut b = BudgetController::new(10.0, &dir);
        // BATCH_WRITE_N is 10; do 3 tiny spends — no auto-flush yet.
        for _ in 0..3 { b.try_spend(0.001).unwrap(); }
        // flush() must persist the current state regardless of the counter.
        b.flush();
        let on_disk: f64 = {
            let raw = std::fs::read_to_string(dir.join("budget.json")).unwrap();
            let v: serde_json::Value = serde_json::from_str(&raw).unwrap();
            v["spent_usd"].as_f64().unwrap()
        };
        assert!((on_disk - 0.003).abs() < 1e-9, "flush must persist, got {on_disk}");
    }

    #[test]
    fn atomic_rename_overwrites_existing_state() {
        let dir = unique_dir("overwrite");
        // Seed the state file with stale content.
        std::fs::write(dir.join("budget.json"), "{\"spent_usd\":0.0,\"day\":\"2000-01-01\"}").unwrap();
        let mut b = BudgetController::new(1.0, &dir);
        // load_state() already discarded the stale day (returning today
        // instead of 2000-01-01), but the file still contains the stale
        // content until we flush. Spend then flush to trigger the atomic
        // rename that overwrites the old content.
        b.try_spend(0.05).unwrap();
        b.flush();
        let raw = std::fs::read_to_string(dir.join("budget.json")).unwrap();
        assert!(
            !raw.contains("2000-01-01"),
            "stale day must have been overwritten, got: {raw}"
        );
    }
}
