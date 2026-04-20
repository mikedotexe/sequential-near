# Architecture

## What the gate is

`gate` is a contract that sits between a user's signed intent and
the target contract that eventually executes it. It accepts NEP-366
`SignedDelegateAction` payloads (borsh-serialized, base64-encoded)
from a whitelisted relayer, holds them under NEP-519 yield/resume
for up to 202 blocks, and dispatches them on a coordinator's
explicit approval. It also supports chained-batch dispatch of N
intents with strict block-monotonic ordering.

## State

```rust
pub struct Gate {
    owner_id: AccountId,                      // init; manages relayers/approver
    approver_id: AccountId,                   // the resume authority
    relayers: LookupSet<AccountId>,           // allowed submit_intent callers
    pending: IterableMap<u64, PendingIntent>, // id -> yield handle + action
    used_nonces: LookupMap<String, u64>,      // "sender|nonce" -> block
    next_intent_id: u64,
    intents_submitted / _dispatched / _rejected: u64,  // counters
    batch_chain_tail: Vec<u64>,               // remaining ids in active batch
    active_batch_id: u64,                     // monotonic batch counter
}
```

`PendingIntent` holds: the yield handle, sender/receiver/method/args
extracted from the verified delegate, plus nonce/expires_at/
submitted_at for observability.

## The single-intent flow

```
 relayer                 gate                      target
 -------                 ----                      ------
 submit_intent(b64) ---> decode + verify sig  
                         store pending
                         yield_id = new_yield(on_intent_resumed)
                         return yielded promise
 [tx1 ends]

 approver
 --------
 resume_intent(id,       remove pending
  approve)        -----> yield_id.resume(signal)
                         [callback fires]
                         on_intent_resumed(approve=true) 
                           Promise::new(receiver)
                             .function_call(method, args)     ----> executes
```

- **Signature verification** happens at `submit_intent` time via
  `env::ed25519_verify` against the NEP-461 hash
  (`sha256(borsh(discriminant_u32) || borsh(DelegateAction))`).
  Fails panic the submit tx. See `docs/wire-format.md`.
- **Replay protection** via `(sender, nonce)` key in `used_nonces`.
  Second submit with the same pair panics.
- **Expiry**: `delegate.max_block_height > env::block_height()` or
  the submit panics.
- **Callback fires exactly once**: either when `resume_intent(...)`
  delivers a payload, or after ~202 blocks when NEP-519 times out.
- **Dispatch data flows via callback_args, not pending**: at
  `submit_intent` time the gate packs `receiver / method / args /
  deposit / gas` into the yield's callback arguments. The callback
  is self-contained and doesn't need to read the `pending` map —
  important because `resume_intent` removes `pending` *before* the
  callback fires, so a state-lookup approach would silently fail on
  the approve path.

## The dispatch tradeoff

near-sdk 5.26.1's `Promise` API exposes these receipt-level actions:
CreateAccount, DeployContract (+ global), FunctionCall (+ weight),
Transfer, Stake, AddKey (full + function-call), DeleteKey,
DeleteAccount, StateInit. It does **not** expose `Delegate`. The
runtime unwraps NEP-366 delegates only at tx-level, not from inside
a contract's Promise chain.

As a consequence: the gate **cannot** spawn a delegate receipt on
behalf of the user. What it can do:

1. Parse the `SignedDelegateAction` as an authentication artifact
   (the signed message proves the user authorized this exact call).
2. Dispatch the intent's `FunctionCall` via
   `Promise::new(receiver).function_call(method, args, deposit, gas)`.

In (2), the dispatched receipt's `predecessor_id = gate`, not
`sender_id`. Target contracts that care about user-attribution will
see the gate as the caller. This is a known tradeoff in exchange
for interposable yield-gating. If near-sdk ever adds
`Promise::delegate`, the gate can swap dispatch mode without
changing the wire format.

In practice, v0.1 applications that use this gate should either:
- Trust the gate as a proxy (target contracts treat gate as an
  authorized agent), OR
- Observe the `intent_submitted` and `intent_dispatched` trace
  events for user-attribution evidence.

### Key-binding limitation

The gate verifies the delegate's signature against the embedded
`public_key` — it does **not** cross-check that `public_key` is
registered as an access key on `sender_id`. A delegate signed with
any keypair whose public_key is embedded will verify; the gate
accepts a "delegate from Alice" as long as whoever signed it also
supplies the pubkey they used to sign.

Real NEP-366 at the runtime level additionally checks that the
embedded pubkey is on `sender_id` with a nonce greater than the
delegate's nonce. Doing that inline from a contract requires a
cross-contract view call (yield on `view_access_key`), which we've
deferred to v0.2. The primary v0.1 defense is the **relayer
whitelist**: only whitelisted relayers can submit at all, so the
gate's trust root is the relayer-operator, not the signer's access
key. Keep this in mind when opening the whitelist on mainnet.

## The chained-batch flow

```
approver
--------
resume_batch_chained([id_a, id_b, id_c])
   set batch_chain_tail = [id_b, id_c]
   remove pending[id_a]
   resume id_a with signal { approve: true, seq: 0, next: Some(id_b) }

                                       on_intent_resumed(id_a, signal)
                                         dispatch_a = Promise(target_a).fc(...)
                                         return dispatch_a
                                           .then(continue_chain(id_b, 1))

                           [block +3]  dispatch_a executes on target_a
                           [block +3]  continue_chain(id_b, 1) fires
                                         pop batch_chain_tail front
                                         remove pending[id_b]
                                         resume id_b with signal { seq: 1, next: Some(id_c) }

                                       on_intent_resumed(id_b, signal)
                                         dispatch_b = Promise(target_b).fc(...)
                                         return dispatch_b
                                           .then(continue_chain(id_c, 2))

                           [block +3]  dispatch_b executes
                           [block +3]  continue_chain(id_c, 2) fires
                                         tail empty after pop
                                         resume id_c with signal { seq: 2, next: None }

                                       on_intent_resumed(id_c, signal)
                                         dispatch_c = Promise(target_c).fc(...)
                                         return dispatch_c   // no chain, tail empty
```

**What this gives you**:

1. **Strict block-monotonic dispatch** — dispatch[i+1] is in a
   strictly later block than dispatch[i], by construction (the
   NEP-519 resume+callback takes at least +2 blocks, the
   continue_chain itself +1).
2. **Transactional sequencing** — intent[i]'s target state-effects
   COMMIT in a block before intent[i+1]'s target executes. This is
   because `continue_chain` is `.then`-chained on dispatch[i], so
   the next resume only fires after dispatch[i] resolves.
3. **Cross-DAG composition** — the chain spans multiple tx DAGs.
   Each submit tx's DAG contains that intent's yielded callback + its
   dispatched receipt + (for all but the last) a `continue_chain`
   receipt. N submit tx hashes + 1 batch tx hash together encode the
   full ordering proof.

## The generic-dispatcher constraint

`continue_chain` must **not** use `#[callback_result]`. That
annotation instructs near-sdk to JSON-deserialize the previous
Promise's return value into a typed arg. But the gate is a generic
dispatcher: inner targets can return `()` (empty bytes), primitives,
structs, anything. `()` returns are empty bytes and fail JSON
parsing with "EOF while parsing". Dropping `#[callback_result]`
makes near-sdk not attempt deserialization; `.then()` still fires
the callback after the previous Promise resolves; the outcome just
isn't consulted.

Verification: `register.set()` returns `()` and chains work;
`ft-shim.transfer()` returns `()` and chains work; if we added a
target returning a struct, chaining still works. See the Phase 5b
history in the research prototype for the bug-fix narrative.

## Owner vs approver vs relayer

- **owner** (init-time, reassignable via `set_approver`): manages
  the relayer whitelist, the approver designation, and emergency
  `reset_batch_tail`. In the demo, owner = master account.
- **approver**: the resume authority. Only `approver_id` can call
  `resume_intent` or `resume_batch_chained`. One approver per gate;
  owner can swap.
- **relayer(s)**: the set of accounts whose `submit_intent` calls
  are accepted. Owner manages via `add_relayer` / `remove_relayer`.
  In the demo, a single `relayer.<master>` account is whitelisted.

The user (signer of the delegate) is not an owner, approver, or
relayer — they're identified only by the `sender_id` and `public_key`
embedded in the signed delegate.

## Gas

- `GAS_YIELD_CALLBACK = 200 Tgas` — reserved at yield time for the
  `on_intent_resumed` callback receipt. Covers state mutation +
  enqueuing the dispatched Promise.
- `GAS_CONTINUE_CHAIN = 60 Tgas` — attached via `with_static_gas`
  to each `continue_chain` chained callback.
- **Inner dispatch gas** — taken from the user-signed delegate's
  `FunctionCallAction.gas` field. The gate passes this through
  verbatim to `Promise::new(receiver).function_call(..., gas)`.
  Users sign for the gas they expect their action to consume.

## Storage keys

Three short byte prefixes, stable across upgrades:

- `b"r"` — relayers set
- `b"p"` — pending map
- `b"n"` — used_nonces map

See `contracts/gate/src/types.rs`. These bytes are load-bearing; a
future upgrade that changes them without migration would orphan
existing pending intents and nonces.

## Trace events

The gate emits `trace:{json}` log lines at every observable moment.
See the table in `CLAUDE.md`. The TypeScript side
(`scripts/src/rpc.ts:extractTraceEvents`) parses these from
receipts_outcome for verification.
