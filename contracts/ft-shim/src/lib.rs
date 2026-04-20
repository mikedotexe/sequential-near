//! ft-shim — minimal fungible-token-like demo target.
//!
//! Not NEP-141 compliant. Just enough to demonstrate that the
//! sequencer's ordering applies to balance mutations: N chained
//! transfers execute in exactly the coordinator's order, and the
//! per-account balance plus the ordered `transfer_log` surface
//! that ordering directly.
//!
//! Shape: owner holds all supply at init; anyone can call
//! `transfer(receiver, amount)` to move balance from the caller to
//! `receiver`. When called via the gate, the caller is the gate
//! (predecessor_id = gate), so the gate's account needs balance —
//! the deploy script pre-funds the gate. This mirrors what a real
//! application-level-dispatch pattern looks like.

use near_sdk::json_types::U128;
use near_sdk::store::LookupMap;
use near_sdk::{env, near, require, AccountId, PanicOnDefault};

const SK_BALANCES: &[u8] = b"b";

#[near(contract_state)]
#[derive(PanicOnDefault)]
pub struct FtShim {
    total_supply: u128,
    balances: LookupMap<AccountId, u128>,
    transfer_log: Vec<(AccountId, AccountId, u128)>,
}

#[near]
impl FtShim {
    #[init]
    pub fn new(owner_id: AccountId, total_supply: U128) -> Self {
        let mut balances: LookupMap<AccountId, u128> = LookupMap::new(SK_BALANCES.to_vec());
        balances.insert(owner_id.clone(), total_supply.0);
        env::log_str(&format!(
            "ft-shim:init:owner:{}:total:{}",
            owner_id, total_supply.0
        ));
        Self {
            total_supply: total_supply.0,
            balances,
            transfer_log: Vec::new(),
        }
    }

    /// Transfer `amount` from `predecessor_account_id()` to `receiver_id`.
    /// No NEP-141 callback / promise plumbing; this is a shim.
    pub fn transfer(&mut self, receiver_id: AccountId, amount: U128) {
        let sender = env::predecessor_account_id();
        require!(amount.0 > 0, "amount must be > 0");
        require!(sender != receiver_id, "cannot transfer to self");

        let sender_bal = self.balances.get(&sender).copied().unwrap_or(0);
        require!(sender_bal >= amount.0, "insufficient balance");

        let receiver_bal = self.balances.get(&receiver_id).copied().unwrap_or(0);
        let new_sender = sender_bal - amount.0;
        let new_receiver = receiver_bal
            .checked_add(amount.0)
            .unwrap_or_else(|| env::panic_str("receiver balance overflow"));

        if new_sender == 0 {
            self.balances.remove(&sender);
        } else {
            self.balances.insert(sender.clone(), new_sender);
        }
        self.balances.insert(receiver_id.clone(), new_receiver);
        self.transfer_log
            .push((sender.clone(), receiver_id.clone(), amount.0));

        env::log_str(&format!(
            "ft-shim:transfer:from:{}:to:{}:amount:{}",
            sender, receiver_id, amount.0
        ));
    }

    pub fn balance_of(&self, account_id: AccountId) -> U128 {
        U128(self.balances.get(&account_id).copied().unwrap_or(0))
    }

    pub fn total_supply(&self) -> U128 {
        U128(self.total_supply)
    }

    pub fn get_transfer_log(&self) -> Vec<(AccountId, AccountId, U128)> {
        self.transfer_log
            .iter()
            .map(|(f, t, a)| (f.clone(), t.clone(), U128(*a)))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use near_sdk::test_utils::VMContextBuilder;
    use near_sdk::testing_env;

    fn owner() -> AccountId {
        "owner.test".parse().unwrap()
    }
    fn alice() -> AccountId {
        "alice.test".parse().unwrap()
    }
    fn bob() -> AccountId {
        "bob.test".parse().unwrap()
    }

    fn ctx(predecessor: AccountId) {
        testing_env!(VMContextBuilder::new()
            .predecessor_account_id(predecessor)
            .build());
    }

    #[test]
    fn init_mints_all_supply_to_owner() {
        ctx(owner());
        let ft = FtShim::new(owner(), U128(1_000));
        assert_eq!(ft.balance_of(owner()).0, 1_000);
        assert_eq!(ft.balance_of(alice()).0, 0);
        assert_eq!(ft.total_supply().0, 1_000);
    }

    #[test]
    fn transfer_moves_balance_and_logs() {
        ctx(owner());
        let mut ft = FtShim::new(owner(), U128(1_000));
        ctx(owner());
        ft.transfer(alice(), U128(300));
        assert_eq!(ft.balance_of(owner()).0, 700);
        assert_eq!(ft.balance_of(alice()).0, 300);
        let log = ft.get_transfer_log();
        assert_eq!(log.len(), 1);
        assert_eq!(log[0].0, owner());
        assert_eq!(log[0].1, alice());
        assert_eq!(log[0].2 .0, 300);
    }

    #[test]
    fn transfer_log_records_order() {
        ctx(owner());
        let mut ft = FtShim::new(owner(), U128(1_000));
        ctx(owner());
        ft.transfer(alice(), U128(100));
        ctx(owner());
        ft.transfer(bob(), U128(200));
        ctx(owner());
        ft.transfer(alice(), U128(50));
        let log = ft.get_transfer_log();
        let amounts: Vec<u128> = log.iter().map(|(_, _, a)| a.0).collect();
        assert_eq!(amounts, vec![100, 200, 50]);
        assert_eq!(ft.balance_of(alice()).0, 150);
        assert_eq!(ft.balance_of(bob()).0, 200);
        assert_eq!(ft.balance_of(owner()).0, 650);
    }

    #[test]
    #[should_panic(expected = "insufficient balance")]
    fn transfer_rejects_overdraw() {
        ctx(owner());
        let mut ft = FtShim::new(owner(), U128(100));
        ctx(alice());
        ft.transfer(bob(), U128(50));
    }

    #[test]
    #[should_panic(expected = "amount must be > 0")]
    fn transfer_rejects_zero_amount() {
        ctx(owner());
        let mut ft = FtShim::new(owner(), U128(100));
        ctx(owner());
        ft.transfer(alice(), U128(0));
    }

    #[test]
    #[should_panic(expected = "cannot transfer to self")]
    fn transfer_rejects_self() {
        ctx(owner());
        let mut ft = FtShim::new(owner(), U128(100));
        ctx(owner());
        ft.transfer(owner(), U128(10));
    }
}
