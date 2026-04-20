# sequential

Sequential ordering of NEAR Intents via yield-gated dispatch of
NEP-366 `SignedDelegateAction` payloads.

## What this is

A production-shaped contract + workflow that accepts user-signed
NEP-366 delegate actions, holds them under NEP-519 yield/resume,
and dispatches them on coordinator approval — optionally as a
chain of N intents that execute in strict block-monotonic order.

Three contracts in the workspace:

- `contracts/gate/` — the signed-intent sequencer. Verifies
  `SignedDelegateAction` signatures, holds intents under yield,
  dispatches on resume.
- `contracts/register/` — non-commutative demo target. Makes
  coordinator ordering directly observable via one view call.
- `contracts/ft-shim/` — minimal FT-like target. Demonstrates
  ordered value movement.

## Key architectural tradeoff

near-sdk 5.26.1's `Promise` API does not expose `Delegate` as a
receipt-level action, so the gate cannot spawn a delegate receipt.
Instead, the gate uses NEP-366 for **signature format and
authorization** (interoperable with existing relayers like
Pagoda's) but **dispatches via gate-as-proxy**
(`Promise::new(receiver).function_call(...)`, `predecessor_id = gate`).

User attribution on targets is therefore lost; this is a
documented tradeoff in exchange for interposable yield-gating.
If and when near-sdk adds `Promise::delegate`, dispatch mode can
swap without changing the wire format.

## Research origin

This project is the brass-tacks follow-on to the research prototype
at `/Users/mikepurvis/near/near-sequencer-demo/experiments/signed-intent-gate/`
(Phases 4 + 5 + 5b). See that repo's README for the feasibility
evidence and the dropped ad-hoc canonical-string signing format
that `sequential` replaces with real NEP-366.

## Quickstart

```
cargo build --release --target wasm32-unknown-unknown --workspace
cargo test --workspace

cd scripts && npm install && cd ..
./scripts/node_modules/.bin/tsc --noEmit -p scripts/tsconfig.json

NEAR_NETWORK=testnet sequential deploy
NEAR_NETWORK=testnet sequential submit --target register --method set --args '{"value":"42"}'
NEAR_NETWORK=testnet sequential sequence --n 3 --target register --permutation 2,0,1
```

## Docs

- `docs/architecture.md` — gate state machine, yield/resume shape
- `docs/wire-format.md` — NEP-366 specifics we rely on
- `docs/verification.md` — the four invariants and how to check them
- `docs/mainnet-readiness.md` — mainnet bootstrap runbook
