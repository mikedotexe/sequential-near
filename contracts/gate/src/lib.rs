//! Signed-intent sequencer gate.
//!
//! Accepts borsh-serialized NEP-366 `SignedDelegateAction` payloads from
//! whitelisted relayers, holds them under NEP-519 yield/resume, and
//! dispatches them on coordinator approval — with optional chained batch
//! ordering (see Phase 5b in the research prototype README).
//!
//! See `docs/architecture.md` at repo root for the full state machine.

use near_sdk::near;

pub mod nep366;

#[near(contract_state)]
#[derive(Default)]
pub struct Gate {}

#[near]
impl Gate {}
