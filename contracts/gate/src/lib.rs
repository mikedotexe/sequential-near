//! Signed-intent sequencer gate — v0.1 single-intent path.
//!
//! Flow: a whitelisted relayer submits a borsh-serialized, base64-
//! encoded `SignedDelegateAction` (NEP-366 wire format) to
//! `submit_intent`. The gate verifies the signature, checks expiry +
//! nonce, stores the pending intent under NEP-519 yield, and returns
//! the yield promise. A coordinator (the `approver_id`) later calls
//! `resume_intent(id, approve)` to either dispatch or reject the
//! intent. On timeout (202 blocks with no resume) the callback also
//! fires with an error, ensuring the pending entry is cleaned up.
//!
//! Chained-batch resume lands in a follow-up commit (see plan); the
//! batch-tail state is already present so `submit_intent`'s storage
//! layout is stable across the two commits.
//!
//! Architectural tradeoff: the dispatched receipt has
//! `predecessor_id = gate`, not `sender_id`, because near-sdk 5.26.1's
//! Promise API does not expose `Delegate` as a receipt-level action.
//! See README.md and docs/architecture.md for the full discussion.

use borsh::BorshDeserialize;
use near_sdk::json_types::{Base64VecU8, U64};
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

        let callback_args = near_sdk::serde_json::json!({
            "intent_id": U64(intent_id),
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
    pub fn resume_intent(&mut self, intent_id: U64, approve: bool) {
        require!(
            env::predecessor_account_id() == self.approver_id,
            "only approver can resume"
        );
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

    // ---- callbacks (private) ----

    /// Yielded callback fires on: (1) approver resume with approve=true
    /// or =false, (2) NEP-519 timeout after ~202 blocks with no resume.
    /// Single-intent flow — batch chaining lands in a follow-up commit.
    #[private]
    pub fn on_intent_resumed(
        &mut self,
        intent_id: U64,
        #[callback_result] signal: Result<ResumeSignal, PromiseError>,
    ) -> PromiseOrValue<()> {
        // Timeout arm: pending entry may still be present (only resume paths
        // remove it in advance). Make cleanup idempotent.
        let pending_at_callback = self.pending.remove(&intent_id.0);

        let approve = match &signal {
            Ok(s) => s.approve,
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

        // Approve path: dispatch via Promise::new(target).function_call(...).
        // We need the target/method/args; these came from the pending entry.
        // Since resume_intent already removed the pending, we captured it
        // above in pending_at_callback (may be None on timeout, but we
        // already returned for that case).
        let target = pending_at_callback.as_ref().map(|p| p.receiver_id.clone());
        let (target, method, args, deposit, gas) = match pending_at_callback {
            Some(p) => (p.receiver_id, p.method, p.args, p.deposit, p.gas),
            None => {
                // resume_intent removed pending BEFORE the resume hit the
                // runtime, and the callback re-entered after another resume
                // or a race. We can't dispatch without the intent data.
                emit_trace(&format!(
                    r#"{{"ev":"intent_resolved_err","id":{},"reason":"pending_missing_on_approve"}}"#,
                    intent_id.0
                ));
                self.intents_rejected += 1;
                let _ = target;
                return PromiseOrValue::Value(());
            }
        };

        emit_trace(&format!(
            r#"{{"ev":"intent_dispatched","id":{},"receiver":"{}","method":"{}"}}"#,
            intent_id.0, target, method
        ));
        self.intents_dispatched += 1;

        let dispatch = Promise::new(target)
            .function_call(method, args, NearToken::from_yoctonear(deposit), Gas::from_gas(gas));
        PromiseOrValue::Promise(dispatch)
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
    /// `(submitted, dispatched, rejected, next_intent_id)`.
    pub fn stats(&self) -> (U64, U64, U64, U64) {
        (
            U64(self.intents_submitted),
            U64(self.intents_dispatched),
            U64(self.intents_rejected),
            U64(self.next_intent_id),
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

        let (submitted, dispatched, rejected, next_id) = gate.stats();
        assert_eq!(submitted.0, 1);
        assert_eq!(dispatched.0, 0);
        assert_eq!(rejected.0, 0);
        assert_eq!(next_id.0, 1);

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
        with_ctx(approver(), 110);
        gate.resume_intent(U64(999), true);
    }
}
