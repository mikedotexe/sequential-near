# Mainnet readiness

This is the runbook for reproducing the sequencer demo on NEAR
mainnet. Mainnet is a first-class deployment target; the four
invariants in `docs/verification.md` are protocol-level claims that
should hold identically on any NEAR network.

## Cost estimate

Per-gate-deploy (once per network):

| line item | approximate cost (NEAR) |
|---|---|
| `alice.<master>` sub-account creation + initial balance | 1 |
| `relayer.<master>` sub-account | 1 |
| `approver.<master>` sub-account | 1 |
| `gate.<master>` sub-account + wasm deploy (~376 KB) + init | 5 |
| `register.<master>` sub-account + wasm deploy (~150 KB) + init | 5 |
| `ft.<master>` sub-account + wasm deploy (~195 KB) + init | 5 |
| `add_relayer` whitelist call | negligible (<0.01) |
| **total one-time bootstrap** | **~18 NEAR** |

NEAR's storage stake is ~1 NEAR per 100 KB; the gate alone needs
~3.76 NEAR for wasm storage, so the per-contract 5 NEAR initial
balance covers storage with comfortable headroom for runtime state
growth (pending map, used_nonces, etc.). The initial testnet deploy
was attempted with 3 NEAR and failed with "wouldn't have enough
balance to cover storage" — 5 NEAR fixed it.

Per-run costs (recoverable from `alice`, `relayer`, `approver` after
`clean`):

| activity | approximate cost (NEAR) |
|---|---|
| `submit_intent` tx (relayer → gate) | 0.002 |
| `resume_intent` tx (approver → gate) | 0.001 |
| `resume_batch_chained` tx (approver → gate) for N=3 | 0.003 |
| NEP-519 yield + callback overhead | bundled into submit gas |
| Dispatched inner FunctionCall (register.set) | 0.001 |
| Dispatched inner FunctionCall (ft-shim.transfer) | 0.002 |

The ~12-NEAR bootstrap assumes you're comfortable leaving ~1 NEAR of
headroom per account for gas. On `clean`, balances flow back to the
master, so the effective cost is storage rent for the duration the
accounts exist (negligible across testing windows).

## Credentials

`~/.near-credentials/mainnet/<master>.json` must exist with a
FullAccess key. The deploy and clean commands assert this via
`assertMasterCredentialPresent()` at startup.

Per-sub-account keys are generated and written into
`~/.near-credentials/mainnet/<sub>.<master>.json` automatically by
`ensureSubAccount`. These keys are what `sequential submit` reads to
sign as alice / relayer / approver.

## FASTNEAR_API_KEY

Not strictly required — FastNEAR's free tier works for small-scale
testing — but recommended for:
- Rate-limit headroom if running `sequence --n 10` or higher.
- Archival queries on the `submit --variant timeout` flow (reading
  a tx's FINAL status after ~210 blocks uses archival RPC).

Set via `.env` or shell export:

```
FASTNEAR_API_KEY=<your-key>
```

## Soft gates

Both `deploy` and `clean` require `--i-know-this-is-mainnet` when
`NEAR_NETWORK=mainnet`. This prevents accidental mainnet runs when
the user meant testnet. `clean` on testnet requires
`--i-know-this-is-testnet` for the same reason (destroying accounts
is always destructive).

## Bootstrap sequence

```bash
# 1) Verify you have master creds
ls ~/.near-credentials/mainnet/mike.near.json

# 2) Deploy
NEAR_NETWORK=mainnet tsx scripts/src/index.ts deploy --i-know-this-is-mainnet

# 3) Exercise a single-intent variant
NEAR_NETWORK=mainnet tsx scripts/src/index.ts submit --variant claim --target register

# 4) Exercise chained batch
NEAR_NETWORK=mainnet tsx scripts/src/index.ts sequence --n 3 --target register --permutation 2,0,1

# 5) Inspect artifacts
cat runs/mainnet/<ts>/sequence-n3-register/record.json
```

Artifacts land under `runs/mainnet/` — separate subtree from
testnet, so cross-network evidence stays cleanly partitioned.

## What to watch for

**Gas underestimates**: if a variant fails with `GasExceeded`,
double-check `GAS_SUBMIT_TGAS` / `GAS_RESUME_TGAS` in
`scripts/src/config.ts` and the `GAS_YIELD_CALLBACK` /
`GAS_CONTINUE_CHAIN` constants in `contracts/gate/src/lib.rs`.
Mainnet's gas cost is protocol-determined and identical to testnet,
so if testnet works, mainnet should too.

**Nonce management**: the runner uses `Date.now()` as the base
nonce. Clock skew or rapid repeated runs can (very rarely) collide.
If a replay is unexpectedly rejected, inspect
`used_nonces` via a view call to confirm.

**Rate limits without API key**: free-tier RPC can 429 on busy runs.
Set `FASTNEAR_API_KEY` if you see 429s in the logs.

**Block-wait for timeout variant**: the timeout variant polls the
block height until +210 blocks have passed. On mainnet (~1s blocks),
this is about 3.5 minutes. Don't panic.

## State hygiene on `gate.<master>`

The gate's `pending` map stores one entry per submitted intent until
either `resume_intent` or the 202-block timeout resolves it. A
malicious relayer with `add_relayer` whitelist access could spam
submits and leak pending entries.

Mitigations in v0.1:
- Relayer whitelist is the primary defense. Only add accounts you
  trust.
- `timeout` path cleans up via the yielded callback — no orphaning
  from unresponsive approvers.
- For emergency mid-chain state where a batch got stuck, owner can
  call `reset_batch_tail` to clear `batch_chain_tail` state.

Not-in-scope mitigations (deferred):
- Per-relayer rate limits or allowance tracking.
- Storage deposits collected from relayers at submit time.
- Automatic pending-sweep after some staleness threshold.

Consider these before opening the relayer whitelist to untrusted
principals on mainnet.

## Updating from testnet to mainnet

If you've already run testnet and want to also have mainnet
evidence committed:

1. Verify testnet is green (`NEAR_NETWORK=testnet` full suite).
2. Bootstrap on mainnet per above.
3. Run a minimal subset on mainnet:
   - `submit --variant claim --target register`
   - `sequence --n 3 --target register --permutation 2,0,1`
4. Commit `runs/mainnet/<ts>/...` to the repo.
5. Cross-link to the testnet run in the commit message or a
   comparative doc (not yet written).

The invariants are per-network; mainnet green is the final check.

## What this repo does NOT attempt

- Decentralized approver coordination (one approver per gate in v0.1).
- Relayer fee accounting (relayer pays gas; no refunds or metering).
- FT-shim is not NEP-141-compliant; use only for demo.
- No multi-action delegates (one FunctionCall per delegate).
- No Transfer / Stake / AddKey inner-action support.

See the "out of scope" list in the repo plan for the full backlog.
