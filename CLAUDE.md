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

Per-contract build: `cargo build --release --target wasm32-unknown-unknown -p gate`.

Note: plain `cargo build --workspace` (no `--target` flag) is
rejected by near-sdk's guard ("Use `cargo near build` instead").
Use `cargo test` for host builds.

## Architecture

### `gate` contract

State: `owner_id`, `approver_id`, `relayers: UnorderedSet`,
`pending: UnorderedMap<u64, PendingIntent>`,
`used_nonces: LookupMap<(AccountId, u64), ()>`, `next_intent_id`,
`batch_chain_tail`, `active_batch_id`.

Owner gates management methods; relayer whitelist gates
`submit_intent`; `approver_id` gates resume methods; callbacks are
`#[private]`.

Public API (see `docs/architecture.md` for full signatures):
- `submit_intent(signed_delegate_base64)` — verify + yield
- `resume_intent(id, approve)` — single-intent resume
- `resume_batch_chained(ids)` — chained batch resume
- views: `get_pending`, `list_pending`, etc.

### Gas budgets (tentative, tune against testnet)

- `GAS_YIELD_CALLBACK`: 150 Tgas
- `GAS_DISPATCH_CALL`: 50 Tgas (passed into the dispatched FunctionCall)
- `GAS_CONTINUE_CHAIN`: 50 Tgas

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

### Trace events

Every observable moment emits a `trace:` log line with shape
`{ev, ...}`:

| event | emitted from | carries |
|---|---|---|
| `intent_submitted` | submit_intent | `id, sender, receiver, nonce` |
| `intent_resumed` | on_intent_resumed | `id, approve, seq, next_id` |
| `intent_dispatched` | on_intent_resumed (approve arm) | `id, receiver, method` |
| `intent_resolved_ok` | on_intent_resumed (approve+dispatch ok) | `id, outcome` |
| `intent_resolved_err` | on_intent_resumed (reject/timeout) | `id, reason` |
| `batch_started` | resume_batch_chained | `batch_id, n, first_id` |
| `chain_continued` | continue_chain | `next_id, next_seq, tail_remaining` |

The Rust `TraceEvent` enum, TS trace parser, and any docs MUST
agree on this vocabulary. Change one, change all.

### Scripts pipeline

`scripts/src/` TypeScript workflow. Entry: `index.ts`.
`config.ts` derives accounts + endpoints from `NEAR_NETWORK`.
`delegate.ts` is the key new piece — borsh-encodes
`SignedDelegateAction` via `@near-js/transactions` and base64-
encodes for the gate.

Every broadcasting command must call `assertChainIdMatches()`
(from `rpc.ts`) before signing.

## Voice principle — vocabulary tracks the contract

Before coining a paraphrase for prose or docs, grep the contract
source. Use the actual method name (`submit_intent`,
`resume_batch_chained`), NEP primitive
(`Promise::new_yield`, `ed25519_verify`), and trace event name
(`intent_dispatched`). Do NOT introduce alternative terms for
existing concepts.

## Scope discipline

Explicit non-goals for v0.1:

- Multi-action delegates (one `FunctionCall` per delegate only).
- Non-FunctionCall inner actions (Transfer / AddKey / etc.).
- Fee model / relayer gas-payment / allowance tracking.
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
confirmation prompts / explicit flags (see
`docs/mainnet-readiness.md`).
