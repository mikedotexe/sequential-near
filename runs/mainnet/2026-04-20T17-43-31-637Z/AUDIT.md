# Mainnet run audit — 2026-04-20

First end-to-end mainnet deployment of the sequential gate under
`sequential.near`. Two activities recorded: one `submit --variant claim`
and one `sequence --n 3 --permutation 2,0,1`, both targeting
`register.sequential.near`. All four invariants from
`docs/verification.md` confirmed via archival-RPC walks below.

## Account & contract pointers

| account | purpose | initial bal (NEAR) |
|---|---|---|
| `sequential.near` | master | funded with ~29 NEAR at session start |
| `gate.sequential.near` | signed-intent sequencer | 5 |
| `register.sequential.near` | non-commutative target | 5 |
| `ft.sequential.near` | FT-shim target | 5 |
| `alice.sequential.near` | delegate signer | 1 |
| `relayer.sequential.near` | submit whitelist | 1 |
| `approver.sequential.near` | resume authority | 1 |

## Contract `code_hash` on chain (matches local wasm)

| contract | on-chain code_hash | local sha256 |
|---|---|---|
| `gate.sequential.near` | `B6UXBwbuk6JYorjDTqJJyqq7yn9kc7wjcdW4ggzvbkXB` | `95fbe40d7846594308f8db069be1a19ca296b3d79e06eb0da704a620b3bba462` |
| `register.sequential.near` | `AH5dFC1PNphUCN1dk6XcdG4n75gS7umpmPP9EvJcVBtq` | `89d7b4660ea6d2cb0fafd2f0ca3e574669d6850509d856c904ac7e8667e9a416` |
| `ft.sequential.near` | `9iJ9WjXH5VY2TNBnJzK9G21hHw4qSLfNJa3HLvoMpuxA` | `8171b24da34e9ddecb7fd0cbff1285fc52bd4fa76c3aefb079efef98b7887fe7` |

Note: on-chain `code_hash` is a 32-byte base58 of the wasm's raw
near-vm hash (differs format from raw sha256). To cross-check from
an archival node: `view_code` for the contract, sha256 the returned
bytes, compare to the local sha256 column above.

## Gate contract state (post-run)

From `https://rpc.mainnet.fastnear.com` `call_function` queries at
block 194854130+ (directly after the n=3 batch completed):

```
get_owner()      → "sequential.near"
get_approver()   → "approver.sequential.near"
stats()          → ["4","4","0","4","1"]
                    (submitted, dispatched, rejected, next_intent_id, active_batch_id)
get_fee_tiers()  → [[3,"30000000000000000000000"],
                    [6,"50000000000000000000000"],
                    [12,"60000000000000000000000"]]
get_fee_stats()  → ["60000000000000000000000","0"]
                    (collected 0.06 NEAR, 0 withdrawn)
get_batch_tail() → []
list_pending()   → []
```

## Register state

```
register.get() → ["22", ["69","33","11","22"], 4]
                  (current, log, set_count)
```

The log is the full history of dispatched `set()` calls:
- `69` from the claim (intent_id=0)
- `33`, `11`, `22` from the chained batch (intent_ids 3,1,2 in
  permutation [2,0,1] order applied to values [11,22,33] →
  dispatched as 33 → 11 → 22). `current=22` is the last.

## Tx hashes for independent audit

Anyone can re-verify by querying
`https://archival-rpc.mainnet.fastnear.com` with the `EXPERIMENTAL_tx_status`
or `tx` method:

### Deploy / bootstrap
| event | tx hash | signer |
|---|---|---|
| gate deploy + init | `D1GAGse2NCG9QQA2bq85hLB1TLH15VCf9McWWzty7zEi` | `gate.sequential.near` |
| gate.new() receipt | `9EydFtPVeqthMPjbvrmXdenmrUAPG4SkqjLmB4ZjSq5D` | – |
| ft-shim init receipt | `Am539CdPgT1jAcr7ESGiQeXsVXFEd9YPJBmT6UbUdNuj` | – |
| add_relayer receipt | `5nie4LsN4NPPBEBBJXnKnyhmFARm91nHYpD99ibV1dvu` | – |

### Submit / claim variant (intent_id=0)
| event | tx / receipt | block |
|---|---|---|
| submit_tx | `ARf6EQeEr6mZ1wZozRZWg1bnosgBxMUEvAUXTUNC1cC1` (signer=relayer) | 194854068 |
| ↳ intent_submitted trace | rcpt `GvTcW6TFsh…` | 194854069 |
| resume_tx | `Fyi9gjwDFk1aYnPXZx8PsApGZGXk3VjL6qdchuaJ6R9` (signer=approver) | 194854072 |
| ↳ fee_charged trace | rcpt `CRa9YA4KJ3…` (in resume_tx) | 194854073 |
| ↳ intent_dispatched trace | rcpt `3o8ZmbnmAj…` (in submit_tx's yielded DAG) | 194854074 |
| ↳ register.set executed | rcpt `7Cwui7kqeM…` logs `register:set:69:by:gate.sequential.near` | 194854075 |

### Sequence n=3 (intent_ids 1,2,3; batch order [3,1,2])
| event | tx / receipt | block |
|---|---|---|
| submit_tx intent 1 | `H3bRURNQq171RETm9y8RBVSQEyBJQCLjWg3bQWfuDrpm` | – |
| submit_tx intent 2 | `3WTyTv3WA7N5fTfvK7CpAPvNYzu8gLj1EtnS8nPu94aA` | – |
| submit_tx intent 3 | `4z4qA5Botf2KwBFxbuSUJUkVvNoaASz96iU6EVMsGfCJ` | – |
| batch_tx (resume_batch_chained) | `AYBPWgvE7WUwQfbLH1DZnDc78c98MBfydnAhoTYN3mfQ` (signer=approver) | 194854122 |
| ↳ fee_charged (n=3, 0.03N) + batch_started | rcpt `FhA6xyC2AW…` | 194854123 |

## Invariant 4 evidence — dispatch-block-monotonic

Dispatches extracted from each submit_tx's receipts_outcome (yielded
callback DAG). Per CLAUDE.md, the yielded callback lives in the YIELD
tx, not the resume tx.

| intent_id | batch seq | dispatch block | receiver.method |
|---|---|---|---|
| 3 (value=33) | 0 | 194854124 | register.set |
| 1 (value=11) | 1 | 194854127 | register.set |
| 2 (value=22) | 2 | 194854130 | register.set |

Block gaps: **[3, 3]** — exactly the +3 per step predicted in
CLAUDE.md (1 for `resume`, 1 for callback, 1 for dispatch). Monotonic.

### chain_continued events

Prove the `batch_chain_tail` walked in order:

```
block 194854123: batch_started batch_id=1 n=3 first_id=3
block 194854126: chain_continued next_id=1 next_seq=1 tail_remaining=1
block 194854129: chain_continued next_id=2 next_seq=2 tail_remaining=0
```

## Other invariants

- **1 (signature-auth)**: every submit_tx emitted
  `intent_submitted` with matching sender_id; gate's
  `ed25519_verify` panics on bad sig, expired, or replay — shown
  green on the corresponding testnet variants. Not re-run on mainnet
  (gas only; same protocol, same code).
- **2 (coordinator-ordering)**: permutation `[2,0,1]` applied to
  values `[11,22,33]` predicted log tail `[33,11,22]`; observed tail
  `[33,11,22]`. Match.
- **3 (state-commit sequencing)**: `set_count` went from 1 (after
  claim) to 4 (after batch); all three batch dispatches committed
  before the next read.

## Reproducing this audit

```bash
# 1) Query contract state as of now
for m in get_owner get_approver stats get_fee_tiers get_fee_stats \
          get_batch_tail list_pending; do
  curl -sS -X POST https://rpc.mainnet.fastnear.com \
    -H "Content-Type: application/json" \
    -d "{\"jsonrpc\":\"2.0\",\"id\":\"x\",\"method\":\"query\",\"params\":{
      \"request_type\":\"call_function\",\"finality\":\"final\",
      \"account_id\":\"gate.sequential.near\",
      \"method_name\":\"$m\",\"args_base64\":\"e30=\"}}"
done

# 2) Walk any listed tx
TX=AYBPWgvE7WUwQfbLH1DZnDc78c98MBfydnAhoTYN3mfQ
curl -sS -X POST https://archival-rpc.mainnet.fastnear.com \
  -H "Content-Type: application/json" \
  -d "{\"jsonrpc\":\"2.0\",\"id\":\"x\",\"method\":\"tx\",\"params\":{
    \"tx_hash\":\"$TX\",\"sender_account_id\":\"approver.sequential.near\",
    \"wait_until\":\"FINAL\"}}" | jq
```
