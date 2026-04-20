//! Gate types: stored pending intents, view projections, resume
//! signals.

use near_sdk::json_types::U64;
use near_sdk::serde::{Deserialize, Serialize};
use near_sdk::{near, AccountId, YieldId};

/// What we store per intent while it's yielded.
#[near(serializers = [borsh])]
#[derive(Clone, Debug)]
pub struct PendingIntent {
    pub yield_id: YieldId,
    pub sender: AccountId,
    pub receiver_id: AccountId,
    pub method: String,
    pub args: Vec<u8>,
    pub deposit: u128,
    pub gas: u64,
    pub nonce: u64,
    pub expires_at_block: u64,
    pub submitted_at_block: u64,
}

/// JSON-safe view projection. Excludes `yield_id` (internal handle).
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(crate = "near_sdk::serde")]
pub struct PendingIntentView {
    pub intent_id: U64,
    pub sender: AccountId,
    pub receiver_id: AccountId,
    pub method: String,
    pub nonce: U64,
    pub expires_at_block: U64,
    pub submitted_at_block: U64,
}

/// Payload that `yield_id.resume(...)` delivers to `on_intent_resumed`
/// via its `#[callback_result]` arg. `sequence_number` and
/// `next_intent_id` are populated only on chained-batch resumes (added
/// in commit 8); single-intent resumes leave them `None`.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(crate = "near_sdk::serde")]
pub struct ResumeSignal {
    pub approve: bool,
    #[serde(default)]
    pub sequence_number: Option<u32>,
    #[serde(default)]
    pub next_intent_id: Option<U64>,
}

/// Storage key prefixes for the gate's map/set members. Keep short
/// since every stored entry carries this prefix. Stability matters
/// across contract upgrades — these bytes are load-bearing for
/// preserving storage across deployments.
pub const SK_RELAYERS: &[u8] = b"r";
pub const SK_PENDING: &[u8] = b"p";
pub const SK_USED_NONCES: &[u8] = b"n";
