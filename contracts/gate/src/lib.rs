//! Signed-intent sequencer gate.
//!
//! Single-intent flow: a whitelisted relayer submits a borsh-
//! serialized, base64-encoded `SignedDelegateAction` (NEP-366 wire
//! format) to `submit_intent`. The gate verifies the signature,
//! checks expiry + nonce, stores the pending intent under NEP-519
//! yield, and returns the yield promise. A coordinator (the
//! `approver_id`) later calls `resume_intent(id, approve)` to either
//! dispatch or reject the intent. On timeout (202 blocks with no
//! resume) the callback also fires with an error, ensuring the
//! pending entry is cleaned up.
//!
//! Chained-batch flow: approver calls `resume_batch_chained(ids)`.
//! The gate resumes `ids[0]` with a signal carrying `next_intent_id =
//! ids[1]`; the yielded callback dispatches the first intent's
//! FunctionCall and `.then()`-chains a `continue_chain(next_id,
//! next_seq+1)` callback. `continue_chain` resumes the next intent
//! with its own threaded next_intent_id, and so on until the tail is
//! empty. Produces strict block-monotonic dispatch (+3 blocks per
//! step) and transactional sequencing (intent[i]'s state commits
//! before intent[i+1]'s target executes).
//!
//! Architectural tradeoff: the dispatched receipt has
//! `predecessor_id = gate`, not `sender_id`, because near-sdk 5.26.1's
//! Promise API does not expose `Delegate` as a receipt-level action.
//! See README.md and docs/architecture.md for the full discussion.

use borsh::BorshDeserialize;
use near_sdk::json_types::{Base64VecU8, U128, U64};
use near_sdk::store::{LookupMap, LookupSet, IterableMap};
use near_sdk::{
    env, near, require, AccountId, Gas, GasWeight, NearToken, PanicOnDefault, Promise,
    PromiseError, PromiseOrValue,
};

pub mod nep366;
pub mod types;

use nep366::SignedDelegateAction;
use types::{PendingIntent, PendingIntentView, ResumeSignal, SK_PENDING, SK_RELAYERS, SK_USED_NONCES};

const GAS_YIELD_CALLBACK: Gas = Gas::from_tgas(200);
const GAS_CONTINUE_CHAIN: Gas = Gas::from_tgas(60);

/// Default fee ladder seeded at `new()`. Owner can replace via
/// `set_fee_tiers`. Caps are batch-size upper bounds (inclusive),
/// amounts are yocto-NEAR.
///
/// - batch of 1..=3   → 0.03 NEAR
/// - batch of 4..=6   → 0.05 NEAR
/// - batch of 7..=12  → 0.06 NEAR
/// - batch of  >12    → rejected
const DEFAULT_FEE_TIERS: [(u32, u128); 3] = [
    (3, 30_000_000_000_000_000_000_000),
    (6, 50_000_000_000_000_000_000_000),
    (12, 60_000_000_000_000_000_000_000),
];

#[near(contract_state)]
#[derive(PanicOnDefault)]
pub struct Gate {
    owner_id: AccountId,
    approver_id: AccountId,
    relayers: LookupSet<AccountId>,
    pending: IterableMap<u64, PendingIntent>,
    used_nonces: LookupMap<String, u64>,
    next_intent_id: u64,
    intents_submitted: u64,
    intents_dispatched: u64,
    intents_rejected: u64,
    /// The tail of an in-flight chained batch. When
    /// `resume_batch_chained(ids)` is called, `batch_chain_tail` is
    /// set to `ids[1..]`. Each `continue_chain` call pops the front.
    /// Must be empty when a new batch starts — the `require!` at the
    /// top of `resume_batch_chained` enforces this.
    batch_chain_tail: Vec<u64>,
    /// Monotonic batch counter for telemetry. 0 = no batch has ever
    /// run; increments at each `resume_batch_chained`. Not used for
    /// ordering — just for observability.
    active_batch_id: u64,
    /// Fee ladder: `(max_batch_size_inclusive, fee_yocto)` sorted by
    /// cap ascending. `resume_*` methods panic if batch size exceeds
    /// the last cap. Owner-rotatable via `set_fee_tiers`.
    fee_tiers: Vec<(u32, u128)>,
    /// Lifetime sum of fees charged (yocto). Monotonic.
    fees_collected_total: u128,
    /// Lifetime sum of fees withdrawn (yocto). Monotonic.
    /// `collected - withdrawn` is the fee-balance available for
    /// withdraw; actual gate-account NEAR may exceed this if external
    /// accounts sent tokens directly.
    fees_withdrawn_total: u128,
}

#[near]
impl Gate {
    #[init]
    pub fn new(owner_id: AccountId, approver_id: AccountId) -> Self {
        emit_trace(&format!(
            r#"{{"ev":"gate_inited","owner":"{}","approver":"{}"}}"#,
            owner_id, approver_id
        ));
        Self {
            owner_id,
            approver_id,
            relayers: LookupSet::new(SK_RELAYERS.to_vec()),
            pending: IterableMap::new(SK_PENDING.to_vec()),
            used_nonces: LookupMap::new(SK_USED_NONCES.to_vec()),
            next_intent_id: 0,
            intents_submitted: 0,
            intents_dispatched: 0,
            intents_rejected: 0,
            batch_chain_tail: Vec::new(),
            active_batch_id: 0,
            fee_tiers: DEFAULT_FEE_TIERS.to_vec(),
            fees_collected_total: 0,
            fees_withdrawn_total: 0,
        }
    }

    // ---- owner-gated management ----

    pub fn add_relayer(&mut self, account_id: AccountId) {
        self.assert_owner();
        self.relayers.insert(account_id.clone());
        emit_trace(&format!(
            r#"{{"ev":"relayer_added","account":"{}"}}"#,
            account_id
        ));
    }

    pub fn remove_relayer(&mut self, account_id: AccountId) {
        self.assert_owner();
        self.relayers.remove(&account_id);
        emit_trace(&format!(
            r#"{{"ev":"relayer_removed","account":"{}"}}"#,
            account_id
        ));
    }

    pub fn set_approver(&mut self, approver_id: AccountId) {
        self.assert_owner();
        self.approver_id = approver_id.clone();
        emit_trace(&format!(
            r#"{{"ev":"approver_set","account":"{}"}}"#,
            approver_id
        ));
    }

    /// Owner-only fee-ladder rotation. Tiers are
    /// `(max_batch_size_inclusive, fee_yocto)`. Caps MUST be strictly
    /// ascending and >0; the list must be non-empty. Amount ordering
    /// is not enforced (a future regime could price larger batches
    /// cheaper; emit an event so observers notice).
    pub fn set_fee_tiers(&mut self, tiers: Vec<(u32, U128)>) {
        self.assert_owner();
        require!(!tiers.is_empty(), "fee tiers must be non-empty");
        let mut prev_cap: u32 = 0;
        for (cap, _) in &tiers {
            require!(*cap > 0, "fee tier cap must be > 0");
            require!(*cap > prev_cap, "fee tier caps must be strictly ascending");
            prev_cap = *cap;
        }
        self.fee_tiers = tiers.iter().map(|(cap, amt)| (*cap, amt.0)).collect();
        emit_trace(&format!(
            r#"{{"ev":"fee_tiers_set","tiers_len":{},"max_cap":{}}}"#,
            self.fee_tiers.len(),
            prev_cap
        ));
    }

    /// Owner-only withdrawal of accumulated fees to `to`. Bounded by
    /// `fees_collected_total - fees_withdrawn_total` (the lifetime
    /// ledger), not by `env::account_balance()` — this keeps storage-
    /// staked NEAR separate from the fee pot.
    pub fn withdraw_fees(&mut self, amount: U128, to: AccountId) -> Promise {
        self.assert_owner();
        let available = self
            .fees_collected_total
            .checked_sub(self.fees_withdrawn_total)
            .unwrap_or(0);
        require!(
            amount.0 <= available,
            format!(
                "insufficient fee balance: requested {}, available {}",
                amount.0, available
            )
        );
        self.fees_withdrawn_total = self
            .fees_withdrawn_total
            .checked_add(amount.0)
            .unwrap_or_else(|| env::panic_str("fees_withdrawn_total overflow"));
        emit_trace(&format!(
            r#"{{"ev":"fees_withdrawn","amount":"{}","to":"{}"}}"#,
            amount.0, to
        ));
        Promise::new(to).transfer(NearToken::from_yoctonear(amount.0))
    }

    /// Owner-only emergency cleanup if a batch failed mid-chain and
    /// left `batch_chain_tail` dirty. Explicit rather than automatic
    /// because silently clearing mid-batch state would mask real bugs.
    pub fn reset_batch_tail(&mut self) {
        self.assert_owner();
        let cleared = self.batch_chain_tail.len();
        self.batch_chain_tail.clear();
        emit_trace(&format!(
            r#"{{"ev":"batch_tail_reset","cleared":{}}}"#,
            cleared
        ));
    }

    // ---- submission ----

    /// Verify a NEP-366 `SignedDelegateAction` and hold it under yield.
    ///
    /// `signed_delegate` is base64(borsh(SignedDelegateAction)). The
    /// caller must be a whitelisted relayer. Returns the yielded
    /// Promise; the relayer can ignore or propagate it.
    pub fn submit_intent(&mut self, signed_delegate: Base64VecU8) -> Promise {
        let relayer = env::predecessor_account_id();
        require!(
            self.relayers.contains(&relayer),
            "caller is not a whitelisted relayer"
        );

        let signed = SignedDelegateAction::try_from_slice(&signed_delegate.0)
            .unwrap_or_else(|e| env::panic_str(&format!("invalid SignedDelegateAction: {}", e)));

        require!(signed.verify(), "signature verification failed");

        let delegate = &signed.delegate_action;
        require!(
            delegate.max_block_height > env::block_height(),
            "intent expired (max_block_height <= current block)"
        );

        let nonce_key = format!("{}|{}", delegate.sender_id, delegate.nonce);
        require!(
            !self.used_nonces.contains_key(&nonce_key),
            "nonce already used for this sender (replay rejected)"
        );

        let fc = delegate
            .require_single_function_call()
            .unwrap_or_else(|e| env::panic_str(e));

        let intent_id = self.next_intent_id;
        self.next_intent_id += 1;
        self.used_nonces.insert(nonce_key, env::block_height());
        self.intents_submitted += 1;

        // Pack all dispatch data into the callback args so on_intent_resumed
        // is self-contained and doesn't need to look up pending state (which
        // the resume path removes before the callback fires).
        let callback_args = near_sdk::serde_json::json!({
            "intent_id": U64(intent_id),
            "receiver": delegate.receiver_id.to_string(),
            "method": fc.method_name.clone(),
            "args": Base64VecU8::from(fc.args.clone()),
            "deposit": U128(fc.deposit),
            "gas": U64(fc.gas),
        });
        let callback_args_bytes = near_sdk::serde_json::to_vec(&callback_args)
            .unwrap_or_else(|_| env::panic_str("callback args serialization failed"));

        let (promise, yield_id) = Promise::new_yield(
            "on_intent_resumed",
            callback_args_bytes,
            GAS_YIELD_CALLBACK,
            GasWeight(1),
        );

        self.pending.insert(
            intent_id,
            PendingIntent {
                yield_id,
                sender: delegate.sender_id.clone(),
                receiver_id: delegate.receiver_id.clone(),
                method: fc.method_name.clone(),
                args: fc.args.clone(),
                deposit: fc.deposit,
                gas: fc.gas,
                nonce: delegate.nonce,
                expires_at_block: delegate.max_block_height,
                submitted_at_block: env::block_height(),
            },
        );

        emit_trace(&format!(
            r#"{{"ev":"intent_submitted","id":{},"sender":"{}","receiver":"{}","method":"{}","nonce":{}}}"#,
            intent_id, delegate.sender_id, delegate.receiver_id, fc.method_name, delegate.nonce
        ));

        promise
    }

    // ---- approver-gated resume ----

    /// Approver decides claim-vs-reject on a single pending intent.
    /// Batch resume is a separate method that lands in a follow-up
    /// commit.
    ///
    /// Payable: the approver attaches >= tier-1 fee (see
    /// `get_fee_tiers`; default 0.03 NEAR). Fee is charged whether
    /// `approve` is true or false — the gate did the verify + yield
    /// work regardless. Timeout path is free naturally (no resume
    /// call, no deposit).
    #[payable]
    pub fn resume_intent(&mut self, intent_id: U64, approve: bool) {
        require!(
            env::predecessor_account_id() == self.approver_id,
            "only approver can resume"
        );
        self.charge_fee(1);
        let pending = self
            .pending
            .remove(&intent_id.0)
            .unwrap_or_else(|| env::panic_str("unknown intent_id"));

        let signal = ResumeSignal {
            approve,
            sequence_number: None,
            next_intent_id: None,
        };
        let payload = near_sdk::serde_json::to_vec(&signal)
            .unwrap_or_else(|_| env::panic_str("resume payload serialization failed"));

        pending
            .yield_id
            .resume(payload)
            .unwrap_or_else(|_| env::panic_str("resume failed (not found or expired)"));

        emit_trace(&format!(
            r#"{{"ev":"intent_resumed","id":{},"approve":{}}}"#,
            intent_id.0, approve
        ));
    }

    /// Approver-submitted ordering of already-yielded intents. Each
    /// intent resumes + dispatches in chain order with strict block-
    /// monotonic spacing (~+3 blocks per step on NEAR). Precondition:
    /// `batch_chain_tail` must be empty (prior batch completed or was
    /// cleared via `reset_batch_tail`).
    ///
    /// Payable: the approver attaches >= the tier fee for
    /// `intent_ids.len()` (see `get_fee_tiers`; default ladder 0.03 /
    /// 0.05 / 0.06 NEAR for batches up to 3 / 6 / 12). Batches larger
    /// than the max tier panic before any state mutation.
    #[payable]
    pub fn resume_batch_chained(&mut self, intent_ids: Vec<U64>) {
        require!(
            env::predecessor_account_id() == self.approver_id,
            "only approver can batch-resume"
        );
        require!(!intent_ids.is_empty(), "batch must not be empty");
        require!(
            self.batch_chain_tail.is_empty(),
            "a chained batch is already in flight; owner must reset_batch_tail if stuck"
        );
        self.charge_fee(intent_ids.len() as u32);

        // Pre-validate every id exists before we mutate state. Catches
        // typos in the approver's batch list without leaving batch_chain_tail
        // dirty or dispatching a partial chain.
        for id in &intent_ids {
            if !self.pending.contains_key(&id.0) {
                env::panic_str(&format!("unknown intent_id {} in batch", id.0));
            }
        }

        self.active_batch_id += 1;
        let batch_id = self.active_batch_id;
        let n = intent_ids.len();
        let first_id = intent_ids[0].0;
        self.batch_chain_tail = intent_ids[1..].iter().map(|u| u.0).collect();
        let next_for_first = self.batch_chain_tail.first().copied().map(U64);

        let pending = self
            .pending
            .remove(&first_id)
            .unwrap_or_else(|| env::panic_str("unknown intent_id for first in batch"));

        let signal = ResumeSignal {
            approve: true,
            sequence_number: Some(0),
            next_intent_id: next_for_first,
        };
        let payload = near_sdk::serde_json::to_vec(&signal)
            .unwrap_or_else(|_| env::panic_str("resume payload serialization failed"));

        pending
            .yield_id
            .resume(payload)
            .unwrap_or_else(|_| env::panic_str("resume failed for first (not found or expired)"));

        emit_trace(&format!(
            r#"{{"ev":"batch_started","batch_id":{},"n":{},"first_id":{}}}"#,
            batch_id, n, first_id
        ));
    }

    /// `.then`-chained off each intent's dispatched FunctionCall. Pops
    /// the next id from `batch_chain_tail` and resumes it. Ignores the
    /// previous dispatch's outcome — a failed inner dispatch does NOT
    /// abort the rest of the chain (the coordinator owns retry
    /// semantics).
    ///
    /// Does NOT use `#[callback_result]` — the gate is a generic
    /// dispatcher; inner targets may return anything (primitives,
    /// `()`, structs). A `#[callback_result]` annotation would attempt
    /// JSON deserialization of the previous Promise's return bytes,
    /// which fails on `()` returns ("EOF while parsing"). `.then()`
    /// still fires this callback after the previous Promise resolves;
    /// the value just isn't consulted.
    #[private]
    pub fn continue_chain(&mut self, next_id: U64, next_seq: u32) -> PromiseOrValue<()> {
        let popped = self
            .batch_chain_tail
            .first()
            .copied()
            .unwrap_or_else(|| env::panic_str("batch_chain_tail empty but expected next_id"));
        require!(
            popped == next_id.0,
            "chain tail mismatch (state drifted from expected next_id)"
        );
        self.batch_chain_tail.remove(0);

        let pending = self
            .pending
            .remove(&next_id.0)
            .unwrap_or_else(|| env::panic_str("unknown next_id in continue_chain"));

        let next_next_id = self.batch_chain_tail.first().copied().map(U64);
        let signal = ResumeSignal {
            approve: true,
            sequence_number: Some(next_seq),
            next_intent_id: next_next_id,
        };
        let payload = near_sdk::serde_json::to_vec(&signal)
            .unwrap_or_else(|_| env::panic_str("resume payload serialization failed"));

        pending
            .yield_id
            .resume(payload)
            .unwrap_or_else(|_| env::panic_str("resume failed in continue_chain"));

        emit_trace(&format!(
            r#"{{"ev":"chain_continued","next_id":{},"next_seq":{},"tail_remaining":{}}}"#,
            next_id.0,
            next_seq,
            self.batch_chain_tail.len()
        ));
        PromiseOrValue::Value(())
    }

    // ---- callbacks (private) ----

    /// Yielded callback fires on: (1) approver resume with approve=true
    /// or =false, (2) NEP-519 timeout after ~202 blocks with no resume.
    /// Handles both single-intent and chained-batch dispatch — when
    /// `signal.next_intent_id` is Some, the dispatched Promise is
    /// `.then`-chained with a `continue_chain(next_id, next_seq+1)`
    /// call that resumes the next intent.
    ///
    /// Dispatch data (receiver/method/args/deposit/gas) is delivered
    /// via the callback_args baked in at `submit_intent` time, not
    /// from the pending map — the resume path removes pending before
    /// triggering this callback, so reading from it would fail on
    /// the approve path. pending.remove here is defensive cleanup
    /// for the timeout arm.
    #[private]
    pub fn on_intent_resumed(
        &mut self,
        intent_id: U64,
        receiver: AccountId,
        method: String,
        args: Base64VecU8,
        deposit: U128,
        gas: U64,
        #[callback_result] signal: Result<ResumeSignal, PromiseError>,
    ) -> PromiseOrValue<()> {
        // Defensive cleanup: resume paths already removed pending, timeout
        // path did not. Unconditional remove is idempotent.
        self.pending.remove(&intent_id.0);

        let (approve, seq, next_intent_id) = match &signal {
            Ok(s) => (s.approve, s.sequence_number, s.next_intent_id),
            Err(e) => {
                emit_trace(&format!(
                    r#"{{"ev":"intent_resolved_err","id":{},"reason":"timeout","detail":"{:?}"}}"#,
                    intent_id.0, e
                ));
                self.intents_rejected += 1;
                return PromiseOrValue::Value(());
            }
        };

        if !approve {
            emit_trace(&format!(
                r#"{{"ev":"intent_resolved_err","id":{},"reason":"rejected"}}"#,
                intent_id.0
            ));
            self.intents_rejected += 1;
            return PromiseOrValue::Value(());
        }

        emit_trace(&format!(
            r#"{{"ev":"intent_dispatched","id":{},"receiver":"{}","method":"{}","seq":{}}}"#,
            intent_id.0,
            receiver,
            method,
            seq.map(|n| n.to_string()).unwrap_or_else(|| "null".into())
        ));
        self.intents_dispatched += 1;

        let dispatch = Promise::new(receiver).function_call(
            method,
            args.0,
            NearToken::from_yoctonear(deposit.0),
            Gas::from_gas(gas.0),
        );

        if let Some(next_id) = next_intent_id {
            let next_seq = seq.map(|s| s + 1).unwrap_or(1);
            let chained = dispatch.then(
                Self::ext(env::current_account_id())
                    .with_static_gas(GAS_CONTINUE_CHAIN)
                    .continue_chain(next_id, next_seq),
            );
            PromiseOrValue::Promise(chained)
        } else {
            PromiseOrValue::Promise(dispatch)
        }
    }

    // ---- views ----

    pub fn get_owner(&self) -> AccountId {
        self.owner_id.clone()
    }

    pub fn get_approver(&self) -> AccountId {
        self.approver_id.clone()
    }

    pub fn is_relayer(&self, account_id: AccountId) -> bool {
        self.relayers.contains(&account_id)
    }

    pub fn get_pending(&self, intent_id: U64) -> Option<PendingIntentView> {
        self.pending.get(&intent_id.0).map(|p| PendingIntentView {
            intent_id,
            sender: p.sender.clone(),
            receiver_id: p.receiver_id.clone(),
            method: p.method.clone(),
            nonce: U64(p.nonce),
            expires_at_block: U64(p.expires_at_block),
            submitted_at_block: U64(p.submitted_at_block),
        })
    }

    pub fn list_pending(&self) -> Vec<U64> {
        self.pending.keys().copied().map(U64).collect()
    }

    /// Roll-up counters for observability. Returns
    /// `(submitted, dispatched, rejected, next_intent_id, active_batch_id)`.
    pub fn stats(&self) -> (U64, U64, U64, U64, U64) {
        (
            U64(self.intents_submitted),
            U64(self.intents_dispatched),
            U64(self.intents_rejected),
            U64(self.next_intent_id),
            U64(self.active_batch_id),
        )
    }

    pub fn get_batch_tail(&self) -> Vec<U64> {
        self.batch_chain_tail.iter().copied().map(U64).collect()
    }

    /// Current fee ladder as `(cap, fee_yocto)` pairs.
    pub fn get_fee_tiers(&self) -> Vec<(u32, U128)> {
        self.fee_tiers
            .iter()
            .map(|(cap, amt)| (*cap, U128(*amt)))
            .collect()
    }

    /// `(collected_total, withdrawn_total)` — both monotonic lifetime
    /// counters in yocto. Available balance = collected - withdrawn.
    pub fn get_fee_stats(&self) -> (U128, U128) {
        (
            U128(self.fees_collected_total),
            U128(self.fees_withdrawn_total),
        )
    }
}

impl Gate {
    fn assert_owner(&self) {
        require!(
            env::predecessor_account_id() == self.owner_id,
            "owner-only"
        );
    }

    /// Smallest tier whose cap is >= `n`. Panics if `n` exceeds the
    /// largest cap (batch too big). Also panics if the tier list is
    /// somehow empty (guarded at setter time so this is unreachable
    /// in practice).
    fn fee_for(&self, n: u32) -> (u32, u128) {
        require!(!self.fee_tiers.is_empty(), "fee tiers not configured");
        for (cap, amount) in &self.fee_tiers {
            if n <= *cap {
                return (*cap, *amount);
            }
        }
        let max_cap = self.fee_tiers.last().map(|(c, _)| *c).unwrap_or(0);
        env::panic_str(&format!(
            "batch size {} exceeds max fee tier ({})",
            n, max_cap
        ));
    }

    /// Read `env::attached_deposit()`, require it cover `required`,
    /// accumulate `required` into `fees_collected_total`, emit a
    /// `fee_charged` trace. Overage (if any) stays on the gate
    /// account balance — no refund in v0.1.
    fn charge_fee(&mut self, n: u32) {
        let (tier_cap, required) = self.fee_for(n);
        let attached = env::attached_deposit().as_yoctonear();
        require!(
            attached >= required,
            format!(
                "insufficient fee: required {} yocto for batch of {}, got {}",
                required, n, attached
            )
        );
        self.fees_collected_total = self
            .fees_collected_total
            .checked_add(required)
            .unwrap_or_else(|| env::panic_str("fees_collected_total overflow"));
        emit_trace(&format!(
            r#"{{"ev":"fee_charged","n":{},"amount":"{}","tier_cap":{}}}"#,
            n, required, tier_cap
        ));
    }
}

fn emit_trace(body: &str) {
    env::log_str(&format!("trace:{}", body));
}

#[cfg(test)]
mod tests {
    use super::*;
    use ed25519_dalek::{Signer, SigningKey};
    use near_sdk::test_utils::VMContextBuilder;
    use near_sdk::testing_env;
    use rand::rngs::OsRng;
    use rand::RngCore;

    fn owner() -> AccountId {
        "owner.test".parse().unwrap()
    }
    fn approver() -> AccountId {
        "approver.test".parse().unwrap()
    }
    fn relayer() -> AccountId {
        "relayer.test".parse().unwrap()
    }
    fn alice() -> AccountId {
        "alice.test".parse().unwrap()
    }
    fn target() -> AccountId {
        "register.test".parse().unwrap()
    }

    fn with_ctx(predecessor: AccountId, block_height: u64) {
        testing_env!(VMContextBuilder::new()
            .predecessor_account_id(predecessor)
            .block_index(block_height)
            .build());
    }

    /// Same as `with_ctx` but attaches a deposit — needed for payable
    /// methods (`resume_intent`, `resume_batch_chained`).
    fn with_ctx_paid(predecessor: AccountId, block_height: u64, deposit_yocto: u128) {
        testing_env!(VMContextBuilder::new()
            .predecessor_account_id(predecessor)
            .block_index(block_height)
            .attached_deposit(NearToken::from_yoctonear(deposit_yocto))
            .build());
    }

    const TIER1_YOCTO: u128 = 30_000_000_000_000_000_000_000; // 0.03 NEAR
    const TIER2_YOCTO: u128 = 50_000_000_000_000_000_000_000; // 0.05 NEAR
    const TIER3_YOCTO: u128 = 60_000_000_000_000_000_000_000; // 0.06 NEAR

    fn make_keypair() -> SigningKey {
        let mut secret = [0u8; 32];
        OsRng.fill_bytes(&mut secret);
        SigningKey::from_bytes(&secret)
    }

    fn sample_signed_delegate(
        kp: &SigningKey,
        sender: AccountId,
        receiver: AccountId,
        nonce: u64,
        max_block_height: u64,
    ) -> Vec<u8> {
        use crate::nep366::*;
        let fc = FunctionCallAction {
            method_name: "set".to_string(),
            args: br#"{"value":"42"}"#.to_vec(),
            gas: 30_000_000_000_000,
            deposit: 0,
        };
        let da = DelegateAction {
            sender_id: sender,
            receiver_id: receiver,
            actions: vec![NonDelegateAction::FunctionCall(fc)],
            nonce,
            max_block_height,
            public_key: Ed25519PublicKey(kp.verifying_key().to_bytes()),
        };
        let hash = da.signed_message_hash();
        let sig = kp.sign(&hash).to_bytes();
        let sda = SignedDelegateAction {
            delegate_action: da,
            signature: Ed25519Signature(sig),
        };
        borsh::to_vec(&sda).unwrap()
    }

    fn init_with_relayer() -> Gate {
        with_ctx(owner(), 100);
        let mut gate = Gate::new(owner(), approver());
        with_ctx(owner(), 100);
        gate.add_relayer(relayer());
        gate
    }

    #[test]
    fn init_stores_owner_and_approver() {
        with_ctx(owner(), 100);
        let gate = Gate::new(owner(), approver());
        assert_eq!(gate.get_owner(), owner());
        assert_eq!(gate.get_approver(), approver());
        assert_eq!(gate.stats().0 .0, 0);
        assert_eq!(gate.stats().4 .0, 0);
    }

    #[test]
    #[should_panic(expected = "owner-only")]
    fn add_relayer_requires_owner() {
        with_ctx(owner(), 100);
        let mut gate = Gate::new(owner(), approver());
        with_ctx(alice(), 101);
        gate.add_relayer(relayer());
    }

    #[test]
    fn add_and_remove_relayer_round_trip() {
        with_ctx(owner(), 100);
        let mut gate = Gate::new(owner(), approver());
        with_ctx(owner(), 101);
        gate.add_relayer(relayer());
        assert!(gate.is_relayer(relayer()));
        with_ctx(owner(), 102);
        gate.remove_relayer(relayer());
        assert!(!gate.is_relayer(relayer()));
    }

    #[test]
    fn set_approver_updates() {
        with_ctx(owner(), 100);
        let mut gate = Gate::new(owner(), approver());
        let new_approver: AccountId = "approver2.test".parse().unwrap();
        with_ctx(owner(), 101);
        gate.set_approver(new_approver.clone());
        assert_eq!(gate.get_approver(), new_approver);
    }

    #[test]
    #[should_panic(expected = "not a whitelisted relayer")]
    fn submit_rejects_non_relayer_caller() {
        let mut gate = init_with_relayer();
        let kp = make_keypair();
        let bytes = sample_signed_delegate(&kp, alice(), target(), 1, 10_000);
        with_ctx(alice(), 110);
        let _ = gate.submit_intent(Base64VecU8::from(bytes));
    }

    #[test]
    #[should_panic(expected = "intent expired")]
    fn submit_rejects_expired_intent() {
        let mut gate = init_with_relayer();
        let kp = make_keypair();
        let bytes = sample_signed_delegate(&kp, alice(), target(), 1, 50);
        with_ctx(relayer(), 100);
        let _ = gate.submit_intent(Base64VecU8::from(bytes));
    }

    #[test]
    #[should_panic(expected = "signature verification failed")]
    fn submit_rejects_bad_signature() {
        let mut gate = init_with_relayer();
        let kp = make_keypair();
        let mut bytes = sample_signed_delegate(&kp, alice(), target(), 1, 10_000);
        let last = bytes.len() - 1;
        bytes[last] ^= 0x01;
        with_ctx(relayer(), 110);
        let _ = gate.submit_intent(Base64VecU8::from(bytes));
    }

    #[test]
    #[should_panic(expected = "replay rejected")]
    fn submit_rejects_replay() {
        let mut gate = init_with_relayer();
        let kp = make_keypair();
        let bytes = sample_signed_delegate(&kp, alice(), target(), 42, 10_000);
        with_ctx(relayer(), 110);
        let _ = gate.submit_intent(Base64VecU8::from(bytes.clone()));
        with_ctx(relayer(), 111);
        let _ = gate.submit_intent(Base64VecU8::from(bytes));
    }

    #[test]
    fn submit_records_pending_and_increments_counters() {
        let mut gate = init_with_relayer();
        let kp = make_keypair();
        let bytes = sample_signed_delegate(&kp, alice(), target(), 1, 10_000);
        with_ctx(relayer(), 110);
        let _ = gate.submit_intent(Base64VecU8::from(bytes));

        let view = gate.get_pending(U64(0)).expect("intent 0 should be pending");
        assert_eq!(view.sender, alice());
        assert_eq!(view.receiver_id, target());
        assert_eq!(view.method, "set");
        assert_eq!(view.nonce.0, 1);
        assert_eq!(view.expires_at_block.0, 10_000);
        assert_eq!(view.submitted_at_block.0, 110);

        let (submitted, dispatched, rejected, next_id, batch_id) = gate.stats();
        assert_eq!(submitted.0, 1);
        assert_eq!(dispatched.0, 0);
        assert_eq!(rejected.0, 0);
        assert_eq!(next_id.0, 1);
        assert_eq!(batch_id.0, 0);

        assert_eq!(gate.list_pending(), vec![U64(0)]);
    }

    #[test]
    fn submit_mints_monotonic_ids_and_preserves_existing() {
        let mut gate = init_with_relayer();
        let kp = make_keypair();

        let a = sample_signed_delegate(&kp, alice(), target(), 1, 10_000);
        with_ctx(relayer(), 110);
        let _ = gate.submit_intent(Base64VecU8::from(a));

        let b = sample_signed_delegate(&kp, alice(), target(), 2, 10_000);
        with_ctx(relayer(), 111);
        let _ = gate.submit_intent(Base64VecU8::from(b));

        assert!(gate.get_pending(U64(0)).is_some());
        assert!(gate.get_pending(U64(1)).is_some());
        assert_eq!(gate.stats().3 .0, 2);
        assert_eq!(gate.stats().4 .0, 0); // no batch started
        let mut listed = gate.list_pending();
        listed.sort_by_key(|u| u.0);
        assert_eq!(listed, vec![U64(0), U64(1)]);
    }

    #[test]
    #[should_panic(expected = "only approver can resume")]
    fn resume_rejects_non_approver() {
        let mut gate = init_with_relayer();
        with_ctx(alice(), 110);
        gate.resume_intent(U64(0), true);
    }

    #[test]
    #[should_panic(expected = "unknown intent_id")]
    fn resume_rejects_unknown_id() {
        let mut gate = init_with_relayer();
        with_ctx_paid(approver(), 110, TIER1_YOCTO);
        gate.resume_intent(U64(999), true);
    }

    // ---- batch ----

    #[test]
    #[should_panic(expected = "only approver can batch-resume")]
    fn batch_rejects_non_approver() {
        let mut gate = init_with_relayer();
        with_ctx(alice(), 110);
        gate.resume_batch_chained(vec![U64(0)]);
    }

    #[test]
    #[should_panic(expected = "batch must not be empty")]
    fn batch_rejects_empty() {
        let mut gate = init_with_relayer();
        with_ctx(approver(), 110);
        gate.resume_batch_chained(vec![]);
    }

    #[test]
    #[should_panic(expected = "unknown intent_id 999 in batch")]
    fn batch_rejects_unknown_first_id() {
        let mut gate = init_with_relayer();
        with_ctx_paid(approver(), 110, TIER1_YOCTO);
        gate.resume_batch_chained(vec![U64(999)]);
    }

    #[test]
    #[should_panic(expected = "unknown intent_id 42 in batch")]
    fn batch_prevalidates_all_ids() {
        // Submit one valid intent (will be id 0), then try to batch [0, 42].
        // Should panic on the second id BEFORE mutating batch_chain_tail or
        // removing pending for intent 0.
        let mut gate = init_with_relayer();
        let kp = make_keypair();
        let bytes = sample_signed_delegate(&kp, alice(), target(), 1, 10_000);
        with_ctx(relayer(), 110);
        let _ = gate.submit_intent(Base64VecU8::from(bytes));

        with_ctx_paid(approver(), 111, TIER1_YOCTO);
        gate.resume_batch_chained(vec![U64(0), U64(42)]);
    }

    #[test]
    fn reset_batch_tail_clears_state() {
        with_ctx(owner(), 100);
        let mut gate = Gate::new(owner(), approver());
        // Forcibly inject a stale tail to simulate mid-chain failure.
        gate.batch_chain_tail = vec![7, 8, 9];
        with_ctx(owner(), 101);
        gate.reset_batch_tail();
        assert!(gate.get_batch_tail().is_empty());
    }

    #[test]
    #[should_panic(expected = "owner-only")]
    fn reset_batch_tail_rejects_non_owner() {
        with_ctx(owner(), 100);
        let mut gate = Gate::new(owner(), approver());
        with_ctx(approver(), 101);
        gate.reset_batch_tail();
    }

    #[test]
    #[should_panic(expected = "chain tail mismatch")]
    fn continue_chain_detects_tail_mismatch() {
        with_ctx(owner(), 100);
        let mut gate = Gate::new(owner(), approver());
        gate.batch_chain_tail = vec![3, 4];
        // Simulate the private callback (testing_env! allows calling private fns
        // because the contract's account is the predecessor by default).
        testing_env!(VMContextBuilder::new()
            .predecessor_account_id(env::current_account_id())
            .block_index(102)
            .build());
        gate.continue_chain(U64(99), 1);
    }

    #[test]
    fn get_batch_tail_roundtrips() {
        with_ctx(owner(), 100);
        let mut gate = Gate::new(owner(), approver());
        gate.batch_chain_tail = vec![10, 20, 30];
        assert_eq!(gate.get_batch_tail(), vec![U64(10), U64(20), U64(30)]);
    }

    // ---- on_intent_resumed direct-invocation tests ----
    //
    // These tests cover the gap that the original bug hid in: the callback
    // is reached only via yield+resume at runtime, which testing_env! can't
    // drive end-to-end. By invoking on_intent_resumed directly with crafted
    // args (as near-sdk's #[callback_result] would do when the resume
    // payload deserializes cleanly), we verify the approve/reject/timeout
    // paths in isolation — including the dispatch-path (which previously
    // silently no-op'd because pending state had been removed by resume).

    fn with_ctx_private(block: u64) {
        // Set predecessor == current_account so #[private] passes.
        let context = VMContextBuilder::new().build();
        let me = context.current_account_id.clone();
        testing_env!(VMContextBuilder::new()
            .current_account_id(me.clone())
            .predecessor_account_id(me)
            .block_index(block)
            .build());
    }

    fn sample_callback_args() -> (AccountId, String, Base64VecU8, U128, U64) {
        (
            target(),
            "set".to_string(),
            Base64VecU8::from(br#"{"value":"42"}"#.to_vec()),
            U128(0),
            U64(30_000_000_000_000),
        )
    }

    #[test]
    fn on_intent_resumed_approve_increments_dispatch() {
        with_ctx(owner(), 100);
        let mut gate = Gate::new(owner(), approver());
        let (rcv, method, args, deposit, gas) = sample_callback_args();

        with_ctx_private(200);
        let _ = gate.on_intent_resumed(
            U64(0),
            rcv,
            method,
            args,
            deposit,
            gas,
            Ok(ResumeSignal {
                approve: true,
                sequence_number: None,
                next_intent_id: None,
            }),
        );

        let (_submitted, dispatched, rejected, _next, _batch) = gate.stats();
        assert_eq!(dispatched.0, 1);
        assert_eq!(rejected.0, 0);
    }

    #[test]
    fn on_intent_resumed_reject_increments_rejected() {
        with_ctx(owner(), 100);
        let mut gate = Gate::new(owner(), approver());
        let (rcv, method, args, deposit, gas) = sample_callback_args();

        with_ctx_private(200);
        let _ = gate.on_intent_resumed(
            U64(0),
            rcv,
            method,
            args,
            deposit,
            gas,
            Ok(ResumeSignal {
                approve: false,
                sequence_number: None,
                next_intent_id: None,
            }),
        );

        let (_submitted, dispatched, rejected, _next, _batch) = gate.stats();
        assert_eq!(dispatched.0, 0);
        assert_eq!(rejected.0, 1);
    }

    #[test]
    fn on_intent_resumed_timeout_increments_rejected() {
        with_ctx(owner(), 100);
        let mut gate = Gate::new(owner(), approver());
        let (rcv, method, args, deposit, gas) = sample_callback_args();

        with_ctx_private(200);
        let _ = gate.on_intent_resumed(
            U64(0),
            rcv,
            method,
            args,
            deposit,
            gas,
            Err(PromiseError::Failed),
        );

        let (_submitted, dispatched, rejected, _next, _batch) = gate.stats();
        assert_eq!(dispatched.0, 0);
        assert_eq!(rejected.0, 1);
    }

    #[test]
    fn on_intent_resumed_approve_does_not_require_pending() {
        // The whole point of the callback_args refactor: the callback can
        // dispatch even when pending was already removed (the normal case
        // on the approve path, since resume_intent removes pending before
        // the callback fires).
        with_ctx(owner(), 100);
        let mut gate = Gate::new(owner(), approver());
        // Explicitly do NOT insert into pending.
        let (rcv, method, args, deposit, gas) = sample_callback_args();

        with_ctx_private(200);
        let _ = gate.on_intent_resumed(
            U64(99),
            rcv,
            method,
            args,
            deposit,
            gas,
            Ok(ResumeSignal {
                approve: true,
                sequence_number: Some(0),
                next_intent_id: None,
            }),
        );

        // Dispatched successfully even though pending was never populated.
        assert_eq!(gate.stats().1 .0, 1);
    }

    // ---- fee mechanism ----

    fn submit_one(gate: &mut Gate, nonce: u64) {
        let kp = make_keypair();
        let bytes = sample_signed_delegate(&kp, alice(), target(), nonce, 10_000);
        with_ctx(relayer(), 110);
        let _ = gate.submit_intent(Base64VecU8::from(bytes));
    }

    /// Run a closure expected to panic inside the mock's
    /// `yield_id.resume(...)` stub (the testing_env mock can't deliver
    /// a yield resume). State mutations that happened BEFORE the
    /// panic — like fee accumulation via `charge_fee` — remain visible
    /// on the Gate struct afterward, so tests can assert against them.
    ///
    /// Any panic whose message doesn't contain "resume failed" is
    /// re-raised so real bugs still surface.
    fn past_mock_yield_fail<F: FnOnce() + std::panic::UnwindSafe>(f: F) {
        let result = std::panic::catch_unwind(f);
        match result {
            Ok(_) => panic!("expected mock yield_id.resume to fail"),
            Err(e) => {
                let msg = e
                    .downcast_ref::<String>()
                    .cloned()
                    .or_else(|| e.downcast_ref::<&str>().map(|s| s.to_string()))
                    .unwrap_or_default();
                if !msg.contains("resume failed") {
                    std::panic::resume_unwind(e);
                }
            }
        }
    }

    #[test]
    fn default_fee_tiers_are_seeded() {
        with_ctx(owner(), 100);
        let gate = Gate::new(owner(), approver());
        let tiers = gate.get_fee_tiers();
        assert_eq!(tiers.len(), 3);
        assert_eq!(tiers[0], (3, U128(TIER1_YOCTO)));
        assert_eq!(tiers[1], (6, U128(TIER2_YOCTO)));
        assert_eq!(tiers[2], (12, U128(TIER3_YOCTO)));
    }

    #[test]
    fn fee_for_maps_size_to_tier() {
        with_ctx(owner(), 100);
        let gate = Gate::new(owner(), approver());
        assert_eq!(gate.fee_for(1), (3, TIER1_YOCTO));
        assert_eq!(gate.fee_for(3), (3, TIER1_YOCTO));
        assert_eq!(gate.fee_for(4), (6, TIER2_YOCTO));
        assert_eq!(gate.fee_for(6), (6, TIER2_YOCTO));
        assert_eq!(gate.fee_for(7), (12, TIER3_YOCTO));
        assert_eq!(gate.fee_for(12), (12, TIER3_YOCTO));
    }

    #[test]
    #[should_panic(expected = "batch size 13 exceeds max fee tier (12)")]
    fn fee_for_panics_above_max_cap() {
        with_ctx(owner(), 100);
        let gate = Gate::new(owner(), approver());
        let _ = gate.fee_for(13);
    }

    #[test]
    #[should_panic(expected = "insufficient fee: required 30000000000000000000000 yocto for batch of 1, got 0")]
    fn resume_intent_rejects_zero_deposit() {
        let mut gate = init_with_relayer();
        submit_one(&mut gate, 1);
        with_ctx(approver(), 111); // no deposit
        gate.resume_intent(U64(0), true);
    }

    #[test]
    #[should_panic(expected = "insufficient fee: required 30000000000000000000000")]
    fn resume_intent_rejects_underpayment() {
        let mut gate = init_with_relayer();
        submit_one(&mut gate, 1);
        with_ctx_paid(approver(), 111, TIER1_YOCTO - 1);
        gate.resume_intent(U64(0), true);
    }

    #[test]
    fn resume_intent_accumulates_tier1_fee() {
        let mut gate = init_with_relayer();
        submit_one(&mut gate, 1);
        with_ctx_paid(approver(), 111, TIER1_YOCTO);
        past_mock_yield_fail(std::panic::AssertUnwindSafe(|| {
            gate.resume_intent(U64(0), true);
        }));
        let (collected, withdrawn) = gate.get_fee_stats();
        assert_eq!(collected.0, TIER1_YOCTO);
        assert_eq!(withdrawn.0, 0);
    }

    #[test]
    fn resume_intent_reject_still_charges_fee() {
        // Gate did the verify+yield work; reject path pays tier-1 too.
        let mut gate = init_with_relayer();
        submit_one(&mut gate, 1);
        with_ctx_paid(approver(), 111, TIER1_YOCTO);
        past_mock_yield_fail(std::panic::AssertUnwindSafe(|| {
            gate.resume_intent(U64(0), false);
        }));
        assert_eq!(gate.get_fee_stats().0 .0, TIER1_YOCTO);
    }

    #[test]
    fn resume_intent_accepts_overpayment_and_keeps_excess() {
        let mut gate = init_with_relayer();
        submit_one(&mut gate, 1);
        with_ctx_paid(approver(), 111, TIER1_YOCTO * 2); // overpay
        past_mock_yield_fail(std::panic::AssertUnwindSafe(|| {
            gate.resume_intent(U64(0), true);
        }));
        // Ledger records only the required tier fee; overage lives on
        // the gate's account balance (no refund in v0.1).
        assert_eq!(gate.get_fee_stats().0 .0, TIER1_YOCTO);
    }

    #[test]
    fn batch_charge_picks_tier_by_size() {
        // Batch of 2 ids → tier-1 (cap 3).
        let mut gate = init_with_relayer();
        submit_one(&mut gate, 1);
        submit_one(&mut gate, 2);
        with_ctx_paid(approver(), 112, TIER1_YOCTO);
        past_mock_yield_fail(std::panic::AssertUnwindSafe(|| {
            gate.resume_batch_chained(vec![U64(0), U64(1)]);
        }));
        assert_eq!(gate.get_fee_stats().0 .0, TIER1_YOCTO);
    }

    #[test]
    #[should_panic(expected = "insufficient fee: required 50000000000000000000000 yocto for batch of 4")]
    fn batch_of_four_requires_tier2_fee() {
        let mut gate = init_with_relayer();
        for i in 1..=4 {
            submit_one(&mut gate, i);
        }
        with_ctx_paid(approver(), 120, TIER1_YOCTO); // tier-1 insufficient for n=4
        gate.resume_batch_chained((0..4).map(U64).collect());
    }

    #[test]
    #[should_panic(expected = "batch size 13 exceeds max fee tier (12)")]
    fn batch_of_thirteen_rejected_before_state_mutation() {
        let mut gate = init_with_relayer();
        // Don't bother submitting — the tier check runs before id validation.
        with_ctx_paid(approver(), 110, TIER3_YOCTO);
        gate.resume_batch_chained((0..13).map(U64).collect());
    }

    #[test]
    #[should_panic(expected = "owner-only")]
    fn set_fee_tiers_rejects_non_owner() {
        with_ctx(owner(), 100);
        let mut gate = Gate::new(owner(), approver());
        with_ctx(alice(), 101);
        gate.set_fee_tiers(vec![(5, U128(1))]);
    }

    #[test]
    #[should_panic(expected = "fee tiers must be non-empty")]
    fn set_fee_tiers_rejects_empty() {
        with_ctx(owner(), 100);
        let mut gate = Gate::new(owner(), approver());
        with_ctx(owner(), 101);
        gate.set_fee_tiers(vec![]);
    }

    #[test]
    #[should_panic(expected = "fee tier caps must be strictly ascending")]
    fn set_fee_tiers_rejects_non_ascending_caps() {
        with_ctx(owner(), 100);
        let mut gate = Gate::new(owner(), approver());
        with_ctx(owner(), 101);
        gate.set_fee_tiers(vec![(5, U128(1)), (3, U128(2))]);
    }

    #[test]
    #[should_panic(expected = "fee tier cap must be > 0")]
    fn set_fee_tiers_rejects_zero_cap() {
        with_ctx(owner(), 100);
        let mut gate = Gate::new(owner(), approver());
        with_ctx(owner(), 101);
        gate.set_fee_tiers(vec![(0, U128(1))]);
    }

    #[test]
    fn set_fee_tiers_rotation_changes_fee_for_resume() {
        // Rotate to a cheaper single-tier ladder, then confirm the new
        // fee is what resume_intent charges.
        let mut gate = init_with_relayer();
        with_ctx(owner(), 100);
        gate.set_fee_tiers(vec![(20, U128(1_000))]);
        submit_one(&mut gate, 1);
        with_ctx_paid(approver(), 112, 1_000);
        past_mock_yield_fail(std::panic::AssertUnwindSafe(|| {
            gate.resume_intent(U64(0), true);
        }));
        assert_eq!(gate.get_fee_stats().0 .0, 1_000);
    }

    #[test]
    #[should_panic(expected = "owner-only")]
    fn withdraw_fees_rejects_non_owner() {
        with_ctx(owner(), 100);
        let mut gate = Gate::new(owner(), approver());
        with_ctx(alice(), 101);
        let _ = gate.withdraw_fees(U128(0), alice());
    }

    #[test]
    #[should_panic(expected = "insufficient fee balance: requested 1, available 0")]
    fn withdraw_fees_rejects_over_available() {
        with_ctx(owner(), 100);
        let mut gate = Gate::new(owner(), approver());
        with_ctx(owner(), 101);
        let _ = gate.withdraw_fees(U128(1), alice());
    }

    #[test]
    fn withdraw_fees_after_resume_updates_ledger() {
        let mut gate = init_with_relayer();
        submit_one(&mut gate, 1);
        with_ctx_paid(approver(), 111, TIER1_YOCTO);
        past_mock_yield_fail(std::panic::AssertUnwindSafe(|| {
            gate.resume_intent(U64(0), true);
        }));

        with_ctx(owner(), 120);
        let _ = gate.withdraw_fees(U128(TIER1_YOCTO), alice());

        let (collected, withdrawn) = gate.get_fee_stats();
        assert_eq!(collected.0, TIER1_YOCTO);
        assert_eq!(withdrawn.0, TIER1_YOCTO);
        // Second full withdrawal should fail — nothing left.
        with_ctx(owner(), 121);
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _ = gate.withdraw_fees(U128(1), alice());
        }));
        assert!(result.is_err(), "over-withdrawal should panic");
    }
}
