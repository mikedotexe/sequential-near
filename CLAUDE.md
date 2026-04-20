# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when
working with code in this repository.

## What this repo is

Production-shaped contract + workflow for sequential ordering of
NEAR Intents. Accepts user-signed NEP-366 `SignedDelegateAction`
payloads, holds them under NEP-519 yield/resume, and dispatches
them on coordinator approval. Supports both single-intent flow and
chained N-intent batches with strict block-monotonic dispatch.

Three contracts: `gate/` (the sequencer), `register/` (ordering-
visible demo target), `ft-shim/` (value-movement demo target).

Both testnet and mainnet are supported deployment targets.

## Research origin — what not to copy

This project is the brass-tacks follow-on to the research prototype at
`/Users/mikepurvis/near/near-sequencer-demo/experiments/signed-intent-gate/`
(Phases 4 + 5 + 5b). That repo's README documents the feasibility
work. **Do not copy-paste contract or script code from it** — the
prototype used an ad-hoc pipe-delimited canonical-string signing
format and DAG-heavy verification that we explicitly leave behind.
Consult it for patterns and findings; re-implement cleanly.

## Key architectural tradeoff

near-sdk 5.26.1's `Promise` API does **not** expose `Delegate` as
a receipt-level action. Only the runtime unwraps delegates, at
tx-level. The gate therefore:

- **Uses NEP-366 for signature format + authorization.** The user
  signs a real `SignedDelegateAction`; a relayer borsh-serializes
  and base64-encodes it; the gate deserializes and verifies. This
  gives wire-format interoperability with existing NEP-366
  relayers (e.g., Pagoda's).
- **Dispatches via gate-as-proxy.** On coordinator approval, the
  gate calls `Promise::new(receiver).function_call(...)`. The
  dispatched receipt has `predecessor_id = gate`, not `user`.

User attribution on target contracts is therefore lost in exchange
for interposable yield-gating. This is documented in
`docs/architecture.md`. If near-sdk adds `Promise::delegate`, we
can switch dispatch mode without changing the wire format.

## Build and test

Cargo workspace of `cdylib`+`rlib` NEAR contracts (near-sdk 5.26.1,
edition 2021). Release profile is tuned for wasm size (`opt-level = "z"`,
`lto = true`, `panic = "abort"`). `rlib` lets `cargo test` link
against the contract code.

Canonical checks:

- `cargo build --release --target wasm32-unknown-unknown --workspace`
- `cargo test --workspace`
- `./scripts/node_modules/.bin/tsc --noEmit -p scripts/tsconfig.json`
- `cd scripts && npm test` — TypeScript unit tests (delegate encoding
  + permutation validation, via `tsx`)

Per-contract build: `cargo build --release --target wasm32-unknown-unknown -p gate`.
Single Rust test: `cargo test --workspace <test_name>` (e.g.,
`cargo test -p gate submit_intent_stores_pending`).

Note: plain `cargo build --workspace` (no `--target` flag) is
rejected by near-sdk's guard ("Use `cargo near build` instead").
Use `cargo test` for host builds.

## Architecture

### `gate` contract

State (`contracts/gate/src/lib.rs`):

```rust
pub struct Gate {
    owner_id: AccountId,
    approver_id: AccountId,
    relayers: LookupSet<AccountId>,
    pending: IterableMap<u64, PendingIntent>,
    used_nonces: LookupMap<String, u64>,       // key = "sender|nonce", val = submit block
    next_intent_id: u64,
    intents_submitted: u64,
    intents_dispatched: u64,
    intents_rejected: u64,
    batch_chain_tail: Vec<u64>,                // remaining ids in active chained batch
    active_batch_id: u64,                      // monotonic batch counter (telemetry)
    fee_tiers: Vec<(u32, u128)>,               // (max_batch_size_incl, yocto) — owner-rotatable
    fees_collected_total: u128,                // lifetime charge sum (yocto)
    fees_withdrawn_total: u128,                // lifetime withdrawal sum (yocto)
}
```

Owner gates management methods; relayer whitelist gates
`submit_intent`; `approver_id` gates resume methods; callbacks
(`on_intent_resumed`, `continue_chain`) are `#[private]`.

Public API (see `docs/architecture.md` for full signatures):
- `submit_intent(signed_delegate_base64)` — verify + yield
- `resume_intent(id, approve)` — **`#[payable]`**; approver attaches tier-1 fee
- `resume_batch_chained(ids)` — **`#[payable]`**; approver attaches size-tier fee
- `set_fee_tiers(tiers)` — owner-only fee-ladder rotation
- `withdraw_fees(amount, to)` — owner-only transfer from accumulated pot
- `reset_batch_tail()` — owner-only recovery if a batch is stuck
- views: `get_pending`, `list_pending`, `stats`, `get_batch_tail`, `get_owner`, `get_approver`, `is_relayer`, `get_fee_tiers`, `get_fee_stats`

### Gas budgets (tune against testnet)

Canonical source is `contracts/gate/src/lib.rs:42-43`.

- `GAS_YIELD_CALLBACK`: 200 Tgas — reserved for `on_intent_resumed`
- `GAS_CONTINUE_CHAIN`: 60 Tgas — reserved for the chained `continue_chain` callback

Per-intent dispatch gas is carried inside the NEP-366 `FunctionCall`
(`delegate.actions[0].gas`) — it is **not** a gate-side constant.
The signer sets it; the gate forwards it unchanged into the
dispatched FunctionCall.

### Cross-tx yield mechanics

As in the research prototype: the yielded callback receipt lives
in the YIELD tx's DAG, not the resume tx's. `Promise::new_yield`
schedules a callback receipt at yield time; `yield_id.resume(...)`
delivers a payload to that already-scheduled receipt. Trace events
emitted by callback code therefore appear on receipt_outcome
entries inside the YIELD tx's `receipts_outcome`.

### Generic dispatcher — no `#[callback_result]`

`continue_chain` **must not** use `#[callback_result]` on the
previous-promise result. The gate dispatches to arbitrary targets
whose return types may be `()`, primitives, structs, etc. A
`#[callback_result]` annotation attempts JSON deserialization of
the previous Promise's return bytes; `()` returns are empty bytes
and fail JSON parsing. `.then()` still fires the callback after
the Promise resolves even without the annotation.

### Dispatch data flows via callback_args, not `pending`

At `submit_intent` time the gate packs `receiver / method / args /
deposit / gas` into the yield's callback-args JSON. The callback
(`on_intent_resumed`) is self-contained and does not read the
`pending` map — important because `resume_intent` removes the
`pending` entry *before* the callback fires. A state-lookup
approach would silently fail on the approve path.

### Trace events

Every observable moment emits a `trace:` log line with shape
`{ev, ...}`. Canonical source is `contracts/gate/src/lib.rs`; this
table tracks what is emitted today:

| event | emitted from | carries |
|---|---|---|
| `gate_inited` | `new` | `owner, approver` |
| `relayer_added` / `relayer_removed` | owner mgmt | `account` |
| `approver_set` | owner mgmt | `account` |
| `batch_tail_reset` | `reset_batch_tail` | `cleared` |
| `intent_submitted` | `submit_intent` | `id, sender, receiver, method, nonce` |
| `intent_resumed` | `resume_intent` | `id, approve` |
| `batch_started` | `resume_batch_chained` | `batch_id, n, first_id` |
| `chain_continued` | `continue_chain` | `next_id, next_seq, tail_remaining` |
| `intent_dispatched` | `on_intent_resumed` (approve arm) | `id, receiver, method, seq` |
| `intent_resolved_err` | `on_intent_resumed` (reject/timeout) | `id, reason, [detail]` |
| `fee_charged` | `resume_intent` / `resume_batch_chained` | `n, amount, tier_cap` |
| `fee_tiers_set` | `set_fee_tiers` | `tiers_len, max_cap` |
| `fees_withdrawn` | `withdraw_fees` | `amount, to` |

Success is implicit via `intent_dispatched`; there is no
`intent_resolved_ok` counterpart today. The Rust emit sites, any
TS parser, and the docs MUST agree on this vocabulary — change one,
change all.

### Scripts pipeline

`scripts/src/` TypeScript workflow (ESM, `tsx` runner). Entry:
`index.ts` with commands `deploy | clean | submit | sequence`.
`config.ts` derives accounts + endpoints from `NEAR_NETWORK`.
`delegate.ts` is the key piece — borsh-encodes
`SignedDelegateAction` via `@near-js/transactions` and base64-
encodes for the gate. `directSender.ts` is a minimal signer that
sidesteps near-api-js quirks for direct FunctionCall tx submission.

Every broadcasting command must call `assertChainIdMatches()`
(from `rpc.ts`) before signing.

Run outputs land in `runs/<network>/<ts>/<cmd>/record.json` —
consult these for tx hashes and empirical invariant checks.

## Voice principle — vocabulary tracks the contract

Before coining a paraphrase for prose or docs, grep the contract
source. Use the actual method name (`submit_intent`,
`resume_batch_chained`), NEP primitive
(`Promise::new_yield`, `ed25519_verify`), and trace event name
(`intent_dispatched`). Do NOT introduce alternative terms for
existing concepts.

## Fee mechanism

The gate charges a tiered, batch-size-indexed NEAR fee at resume
time. The approver attaches NEAR as deposit on `resume_intent` or
`resume_batch_chained`; the gate reads `env::attached_deposit()`,
looks up the smallest tier whose cap is >= batch size, and
`require!`s the deposit covers it. The fee is charged to a
monotonic ledger counter (`fees_collected_total`); accumulated
NEAR lives on the gate's account balance until the owner calls
`withdraw_fees(amount, to)`.

Canonical source: `contracts/gate/src/lib.rs` — `DEFAULT_FEE_TIERS`
constant, `fee_for` / `charge_fee` helpers, `set_fee_tiers`,
`withdraw_fees`. TS mirror: `scripts/src/config.ts:FEE_TIERS` +
`feeForBatchSize(n)`.

**Default ladder (seeded in `new()`, owner-rotatable):**

| batch size | fee |
|---|---|
| 1..=3   | 0.03 NEAR |
| 4..=6   | 0.05 NEAR |
| 7..=12  | 0.06 NEAR |
| >12     | panics (`"batch size N exceeds max fee tier (12)"`) |

**Collection points:**

- `resume_intent` charges tier-1 regardless of `approve` (the gate
  did the verify+yield work either way).
- `resume_batch_chained(ids)` charges the tier that covers
  `ids.len()`. Tier-too-big panics before any state mutation.
- Timeout is free naturally: no resume call means no attached
  deposit.
- Overpayment is accepted; only the required amount is ledgered,
  the excess lands on the gate's raw account balance. No refund
  in v0.1.

**Inspiration — why this shape:** `intents.near`
(`github.com/near/intents`, `crates/defuse-core/src/`) charges fees
via post-batch reconciliation — `Deltas::finalize()` folds a
`Pips`-rate fee credit into the sum-to-zero invariant, read from
two owner-rotatable state values (`StateView::fee()` and
`fee_collector()`). We borrow the **owner-rotatable, state-driven
(not constant) rate** and the **ledger-entry-first, transfer-later**
separation, but not the per-token closure machinery — the gate is
a generic delegate-action sequencer, not a token-balance Verifier,
so a sum-to-zero invariant doesn't apply. Batch-size tiers replace
Pips; attached-deposit checks replace balance-delta accounting.

**Keep invariant — generic-dispatcher property:** fee logic never
introspects the inner FunctionCall's target/method/args. Adding
per-target fee tables or Pips-on-FT-transfers would break this.

## Scope discipline

Explicit non-goals for v0.1 (fee mechanism is a deliberate
research direction, not a non-goal):

- Multi-action delegates (one `FunctionCall` per delegate only).
- Non-FunctionCall inner actions (Transfer / AddKey / etc.).
- Oauth / fastauth integration.
- Parallel resume (only chained).
- Rejection-with-skip mid-batch (reject = batch-level abort).
- Event indexer (trace events land on-chain; no separate indexer).
- Visualizations / Manim scenes.
- Public API documentation beyond `docs/architecture.md` +
  `docs/wire-format.md`.

## Network support

`NEAR_NETWORK=testnet|mainnet`. `config.ts` derives account
prefixes, RPC endpoints (FastNEAR), expected chain_id. Both
networks are first-class; mainnet commands are soft-gated behind
confirmation prompts / explicit flags (`--i-know-this-is-mainnet`
on `deploy` and `clean`, `--i-know-this-is-testnet` on testnet
`clean`). See `docs/mainnet-readiness.md`.
