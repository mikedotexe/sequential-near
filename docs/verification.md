# Verification

Four invariants characterize the gate's correctness. Each has a
test hook on-chain (trace event or view state) that can be checked
without walking the receipt DAG.

## Invariant 1: signature-auth

**Claim**: A submitted intent is dispatched only if the embedded
signature is valid for the embedded public key over the NEP-461 hash
of the serialized DelegateAction; the pubkey has not been tampered
with; the `max_block_height` is in the future; and the
`(sender, nonce)` pair has not been used before on this gate.

**Check**: The gate's `submit_intent` panics on any failure mode.
The `sequential submit` runner exercises the six variants:

| variant  | expected outcome |
|----------|------------------|
| claim    | submit succeeds; `intent_submitted` trace; dispatch on approve |
| reject   | submit succeeds; `intent_submitted` trace; `intent_resolved_err reason=rejected` on `approve=false` |
| timeout  | submit succeeds; no resume; after 202 blocks `intent_resolved_err reason=timeout` trace fires |
| bad-sig  | submit_intent panics with "signature verification failed" |
| expired  | submit_intent panics with "intent expired" |
| replay   | first submit ok; second with same nonce panics with "replay rejected" |

**Run**:
```
NEAR_NETWORK=testnet sequential submit --variant claim
NEAR_NETWORK=testnet sequential submit --variant reject
NEAR_NETWORK=testnet sequential submit --variant timeout   # ~3.5 min wait
NEAR_NETWORK=testnet sequential submit --variant bad-sig
NEAR_NETWORK=testnet sequential submit --variant expired
NEAR_NETWORK=testnet sequential submit --variant replay
```

Each writes `runs/testnet/<ts>/submit-<variant>-<target>/record.json`
with the tx hashes and the actual error message.

## Invariant 2: coordinator-ordering

**Claim**: In a chained batch, dispatches hit the target in exactly
the order the approver specified in `intent_ids`, regardless of the
order in which `submit_intent` was called.

**Check**: Compare target state after the batch against the
permutation-derived expected. No DAG walking — one view call.

For the `register` target: `register.get()` returns `(current, log,
set_count)`. The last `n` entries of `log` should match the values
in permutation order; `current` should match the last value in that
order.

For `ft-shim`: `ft-shim.get_transfer_log()` ordered tail matches the
permuted amounts.

**Run**:
```
sequential sequence --n 3 --target register --permutation identity
sequential sequence --n 3 --target register --permutation 2,0,1
sequential sequence --n 5 --target ft-shim --permutation random
```

Each writes `runs/testnet/<ts>/sequence-n<n>-<target>/record.json`
with `match: true|false`. Exit code 2 on mismatch.

## Invariant 3: state-commit sequencing

**Claim**: Intent[i+1]'s target execution sees intent[i]'s state
commits. That is: when dispatch[i+1] runs, the block containing
dispatch[i]'s receipt has already finalized, so reads in
dispatch[i+1]'s FunctionCall see dispatch[i]'s writes.

**Check**: For `register`, after a batch of N, `set_count == n`
(cumulative if this target is reused across runs) and `log.length`
grew by exactly N. If any dispatch had missed committing before the
next, some would race and the counter / log would be short.

For `ft-shim`, the receiver balance equals the sum of all N
transfers (each transfer subtracts from caller = gate, adds to
receiver). If any dispatch had raced past a prior's commit, the
receiver-side add might double-count or miss.

**Run**: Same as invariant 2. Look at the `state.post` in
record.json.

## Invariant 4: dispatch-block-monotonic

**Claim**: In a chained batch, dispatch[i+1]'s block height is
strictly greater than dispatch[i]'s. The per-step quantum is exactly
+3 blocks on NEAR (1 block for `resume`, 1 for the callback, 1 for
the dispatch).

**Check**: Not observability-critical in crisp mode (invariants 2
and 3 already rule out racing). For empirical record-keeping we
capture the dispatch DAG on one published run per (network, N) and
commit the block heights to the repo as evidence.

**Not implemented in v0.1** as a runner step — the research
prototype's Phase 5 has DAG-walking code if you want to generate
this evidence manually; for v0.1 the invariant is secondary to 2
and 3.

## Running the full suite

Workspace unit tests:

```
cargo test --workspace
# 59 tests across nep366 + gate + register + ft-shim
```

TypeScript unit tests:

```
cd scripts && npm test
# delegate encoding + permutation validation
```

End-to-end on testnet (requires master credentials in
`~/.near-credentials/testnet/<master>.json`):

```
NEAR_NETWORK=testnet sequential deploy
NEAR_NETWORK=testnet sequential submit --variant claim
NEAR_NETWORK=testnet sequential submit --variant reject
NEAR_NETWORK=testnet sequential submit --variant bad-sig
NEAR_NETWORK=testnet sequential submit --variant expired
NEAR_NETWORK=testnet sequential submit --variant replay
NEAR_NETWORK=testnet sequential submit --variant timeout  # wait ~3.5min
NEAR_NETWORK=testnet sequential sequence --n 3 --target register --permutation 2,0,1
NEAR_NETWORK=testnet sequential sequence --n 3 --target ft-shim --permutation 1,2,0
```

Then on mainnet with `NEAR_NETWORK=mainnet` and the
`--i-know-this-is-mainnet` flag on `deploy`. See
`docs/mainnet-readiness.md` for cost estimates and soft-gate details.
