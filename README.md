# sequential

A NEAR contract + workflow for **ordering signed intents on-chain**.
Users sign NEP-366 `SignedDelegateAction` payloads once; a
coordinator decides *when* and *in what order* they dispatch.

## What it does

The `gate` contract sits between a user's signed intent and the
contract that eventually executes it. It:

1. accepts a user-signed `SignedDelegateAction` from a whitelisted
   relayer (wire-compatible with existing NEP-366 relayer stacks);
2. verifies the ed25519 signature against the NEP-461 hash, checks
   nonce and expiry, and holds the intent under an NEP-519 yield;
3. dispatches the inner `FunctionCall` when a coordinator approves
   — either one intent at a time, or as a chain of N intents whose
   on-chain dispatch blocks are strictly monotonic.

Three contracts ship in the workspace:

| contract | role |
|---|---|
| `gate` | signed-intent sequencer (verify → yield → dispatch) |
| `register` | non-commutative demo target (`set(value)` with ordered log) |
| `ft-shim` | FT-like demo target for ordered value movement |

## Live on NEAR mainnet

Deployed at **`gate.sequential.near`** on 2026-04-20. Every
observable moment emits a `trace:{...}` log line that can be read
directly from archival RPC — no indexer required.

| activity | tx | evidence |
|---|---|---|
| gate init | `D1GAGse2NCG9QQA2bq85hLB1TLH15VCf9McWWzty7zEi` | `gate_inited` trace |
| single-intent claim | resume `Fyi9gjwDFk1aYnPXZx8PsApGZGXk3VjL6qdchuaJ6R9` | `register.set(69)` executed |
| chained batch of 3 | batch resume `AYBPWgvE7WUwQfbLH1DZnDc78c98MBfydnAhoTYN3mfQ` | dispatches at blocks 194854124 / 194854127 / 194854130 — strictly +3 per step |

Full receipt walk, block heights, trace events, and reproduction
commands in
[`runs/mainnet/2026-04-20T17-43-31-637Z/AUDIT.md`](runs/mainnet/2026-04-20T17-43-31-637Z/AUDIT.md).

## How the chained batch works

```
 approver                      gate                         target
 --------                      ----                         ------
 resume_batch_chained(ids) --> charge fee (attached NEAR)
                               pending[ids[0]].yield_id.resume(signal)
                                  ↓ (1 block)
                               callback fires
                                  Promise::new(target).function_call(...)  -→ executes
                                  .then(continue_chain(ids[1], seq+1))
                                  ↓ (1 block)
                               continue_chain resumes ids[1] ...
```

Each step takes exactly three blocks (resume → callback → dispatch),
which gives the sequencer a provable `block[i+1] > block[i]` guarantee
on the target chain — and the target always sees intent `i`'s state
committed before intent `i+1` runs.

## Fees

`resume_intent` and `resume_batch_chained` are `#[payable]`; the
approver attaches NEAR as deposit, indexed by batch size:

| batch size | fee per resume |
|---|---|
| 1..=3 | 0.03 NEAR |
| 4..=6 | 0.05 NEAR |
| 7..=12 | 0.06 NEAR |
| >12 | rejected |

Tiers are owner-rotatable via `set_fee_tiers`. Accumulated fees sit
on the gate account until the owner calls `withdraw_fees(amount, to)`.

## Key architectural tradeoff

NEAR's `Promise` API does not expose `Delegate` as a receipt-level
action — only the runtime unwraps delegates, at tx level. The gate
therefore:

- uses **NEP-366 for signature format + authorization** (real
  `SignedDelegateAction`; wire-compatible with existing relayers);
- dispatches **via gate-as-proxy** (`Promise::new(target).function_call(...)`).

The dispatched receipt has `predecessor_id = gate`, not the user.
User attribution on the target is lost in exchange for interposable
yield-gating, signature-level replay protection, and the
block-monotonic ordering guarantee. If `Promise::delegate` is ever
added to near-sdk, dispatch mode can swap without changing the wire
format.

Full discussion in [`docs/architecture.md`](docs/architecture.md).

## Quickstart

Prereqs: Rust with `wasm32-unknown-unknown` + nightly `rust-src`
(see `rust-toolchain.toml`); Node 20+; a funded NEAR account with a
FullAccess key at `~/.near-credentials/<network>/<account>.json`.

```bash
# Build
cargo build --release --target wasm32-unknown-unknown --workspace
cargo test --workspace
cd scripts && npm install && cd ..

# Deploy (testnet)
NEAR_NETWORK=testnet MASTER_ACCOUNT_ID=<your.testnet> \
  ./scripts/node_modules/.bin/tsx scripts/src/index.ts deploy

# Single-intent claim
NEAR_NETWORK=testnet MASTER_ACCOUNT_ID=<your.testnet> \
  ./scripts/node_modules/.bin/tsx scripts/src/index.ts \
  submit --variant claim --target register

# Chained batch of 3, permutation [2,0,1]
NEAR_NETWORK=testnet MASTER_ACCOUNT_ID=<your.testnet> \
  ./scripts/node_modules/.bin/tsx scripts/src/index.ts \
  sequence --n 3 --target register --permutation 2,0,1
```

For mainnet, swap `NEAR_NETWORK=mainnet` and add
`--i-know-this-is-mainnet` to `deploy` / `clean` (see
[`docs/mainnet-readiness.md`](docs/mainnet-readiness.md) for cost
estimate and bootstrap runbook).

Each run writes a `runs/<network>/<timestamp>/.../record.json` with
tx hashes, fee charges, and pre/post target state — the
observability substrate verification depends on.

## Verification

Four invariants, each checkable against a single view or trace
event — no DAG walking required:

1. **signature-auth** — submit panics on bad sig / expired / replay.
2. **coordinator-ordering** — target log tail matches the
   permutation the approver specified.
3. **state-commit sequencing** — `register.set_count` advances by
   exactly N across a batch of N.
4. **dispatch-block-monotonic** — `block[i+1] - block[i] == 3`.

See [`docs/verification.md`](docs/verification.md) for the full
runbook and [`docs/architecture.md`](docs/architecture.md) for the
gate state machine.

## Scope — what v0.1 does NOT do

- Multi-action delegates (one `FunctionCall` per delegate).
- Non-`FunctionCall` inner actions (Transfer / AddKey / Stake).
- Parallel resume (only chained).
- Rejection-with-skip mid-batch (reject = batch-level abort).
- Decentralized approver coordination.
- FT-shim is a demo target, not NEP-141-compliant.

## License

Dual-licensed MIT OR Apache-2.0.
