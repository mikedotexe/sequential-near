//! Register — non-commutative demo target.
//!
//! A last-writer-wins u64 register with an ordered log of every
//! `set` it's ever seen. Because both `current` and `log` depend on
//! the order of calls, dispatching N `set`s in different orders
//! yields different observable state. This makes the sequencer
//! gate's ordering choice directly visible from one view call to
//! `get()`, without DAG walking.
//!
//! See docs/verification.md for how this target powers the
//! coordinator-ordering invariant check.

use near_sdk::json_types::U64;
use near_sdk::{env, near};

#[near(contract_state)]
#[derive(Default)]
pub struct Register {
    current: u64,
    log: Vec<u64>,
    set_count: u32,
}

#[near]
impl Register {
    /// Overwrite `current`, append to `log`, increment `set_count`.
    /// Emits a `register:set:<value>:by:<caller>` log for trace
    /// correlation with the gate's trace events.
    pub fn set(&mut self, value: U64) {
        env::log_str(&format!(
            "register:set:{}:by:{}",
            value.0,
            env::predecessor_account_id()
        ));
        self.current = value.0;
        self.log.push(value.0);
        self.set_count += 1;
    }

    /// Returns `(current, log, set_count)`. One view call surfaces
    /// both final state and full history — enough to verify the
    /// coordinator's ordering choice without DAG inspection.
    pub fn get(&self) -> (U64, Vec<U64>, u32) {
        (
            U64(self.current),
            self.log.iter().copied().map(U64).collect(),
            self.set_count,
        )
    }

    /// Test-only helper. Not access-controlled; the demo workflow
    /// destroys and re-deploys the account between test runs, so
    /// admin-gating this here would be security theater.
    pub fn reset(&mut self) {
        env::log_str("register:reset");
        self.current = 0;
        self.log.clear();
        self.set_count = 0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use near_sdk::test_utils::VMContextBuilder;
    use near_sdk::testing_env;
    use near_sdk::AccountId;

    fn alice() -> AccountId {
        "alice.test".parse().unwrap()
    }

    fn ctx(predecessor: AccountId) {
        testing_env!(VMContextBuilder::new()
            .predecessor_account_id(predecessor)
            .build());
    }

    #[test]
    fn default_state_is_empty() {
        let r = Register::default();
        let (current, log, count) = r.get();
        assert_eq!(current.0, 0);
        assert!(log.is_empty());
        assert_eq!(count, 0);
    }

    #[test]
    fn set_updates_current_and_appends_log() {
        ctx(alice());
        let mut r = Register::default();
        r.set(U64(11));
        r.set(U64(22));
        r.set(U64(33));
        let (current, log, count) = r.get();
        assert_eq!(current.0, 33);
        assert_eq!(log, vec![U64(11), U64(22), U64(33)]);
        assert_eq!(count, 3);
    }

    #[test]
    fn set_is_non_commutative_in_final_state() {
        ctx(alice());
        let mut a = Register::default();
        a.set(U64(11));
        a.set(U64(22));
        a.set(U64(33));
        let mut b = Register::default();
        b.set(U64(33));
        b.set(U64(11));
        b.set(U64(22));
        assert_ne!(a.get(), b.get());
        assert_eq!(a.get().0 .0, 33);
        assert_eq!(b.get().0 .0, 22);
    }

    #[test]
    fn reset_clears_all_fields() {
        ctx(alice());
        let mut r = Register::default();
        r.set(U64(11));
        r.set(U64(22));
        r.reset();
        let (current, log, count) = r.get();
        assert_eq!(current.0, 0);
        assert!(log.is_empty());
        assert_eq!(count, 0);
    }
}
