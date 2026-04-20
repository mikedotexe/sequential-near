# NEP-366 Wire Format

This document specifies exactly which parts of NEP-366 / NEP-461 the
`gate` contract implements, and how the Rust and TypeScript sides
stay byte-for-byte aligned.

## Signed message scheme (NEP-461)

To sign a `DelegateAction`, the user computes:

```
hash = sha256( borsh(discriminant: u32) || borsh(DelegateAction) )
```

then signs that 32-byte hash with their ed25519 key. The gate's
`submit_intent` reconstructs this hash from the delegate bytes and
passes it to `env::ed25519_verify(signature, hash, public_key)`.

### Discriminant

```
MIN_ON_CHAIN_DISCRIMINANT = 1u32 << 30 = 1_073_741_824
NEP_366_DELEGATE          = MIN_ON_CHAIN_DISCRIMINANT + 366
                          = 1_073_742_190
```

Borsh encodes u32 little-endian, so the four leading bytes of the
signed message are `0x6E 0x01 0x00 0x40`. Verified in
`contracts/gate/src/nep366.rs::tests::discriminant_borsh_bytes_little_endian`.

This discriminant prefix is the NEP-461 anti-confusion measure: a
signature over a NEP-366 delegate message cannot be replayed as a
signature over a conventional transaction (which has a different
leading byte pattern — the short u32 length of the initial AccountId
string).

## `DelegateAction` layout

Byte-for-byte matches `near_primitives::action::delegate::DelegateAction`:

```rust
#[derive(BorshSerialize, BorshDeserialize)]
pub struct DelegateAction {
    pub sender_id: AccountId,       // borsh String (u32 LE length + UTF-8 bytes)
    pub receiver_id: AccountId,
    pub actions: Vec<NonDelegateAction>,  // u32 LE len + elems
    pub nonce: u64,                 // 8 bytes LE
    pub max_block_height: u64,      // 8 bytes LE
    pub public_key: Ed25519PublicKey,
}
```

**Order matters**: changing the field order changes the wire hash
and breaks signature verification. Do not reorder.

## `SignedDelegateAction` layout

```rust
#[derive(BorshSerialize, BorshDeserialize)]
pub struct SignedDelegateAction {
    pub delegate_action: DelegateAction,
    pub signature: Ed25519Signature,
}
```

This is what `submit_intent` accepts as base64-encoded bytes.

## Ed25519 public key / signature

Both follow the NEAR convention: a one-byte tag (0 = ed25519, 1 =
secp256k1) followed by the raw key/sig bytes.

```rust
struct Ed25519PublicKey(pub [u8; 32]);  // wire: 0u8 || 32 bytes
struct Ed25519Signature(pub [u8; 64]);  // wire: 0u8 || 64 bytes
```

v0.1 rejects any key/signature with a non-zero tag at
`BorshDeserialize` time with a clear error message. Secp256k1
support is not in v0.1 scope.

## `NonDelegateAction` — v0.1 scope

The inner `actions: Vec<NonDelegateAction>` is where NEP-366's
combination of inner actions lives. For v0.1, the gate accepts ONLY
the `FunctionCall` variant (tag byte 2):

```rust
pub enum NonDelegateAction {
    FunctionCall(FunctionCallAction),  // tag: 2
}

pub struct FunctionCallAction {
    pub method_name: String,
    pub args: Vec<u8>,
    pub gas: u64,
    pub deposit: u128,
}
```

Variants deserialize:
- Tag 2 → `FunctionCall` — supported.
- Tag 8 → error: "nested DelegateAction forbidden (NEP-366)".
  NEP-366 forbids nested delegates; the
  `NonDelegateAction` wrapper in near-primitives enforces this at
  parse time, and so do we.
- Any other tag (0, 1, 3, 4, 5, 6, 7) → error: "action variant N
  not supported in v0.1 (only FunctionCall)".

This constrains what signers can include in a delegate targeting our
gate without breaking wire-format compatibility. A fuller version
could extend `NonDelegateAction` to parse other variants; for v0.1,
the single-FunctionCall constraint matches the MVP scope.

Additional constraint in `submit_intent`: `actions.len() == 1`. A
delegate with two FunctionCalls would successfully deserialize but
gets rejected at validate time with
"v0.1 gate requires exactly one action per delegate". See
`DelegateAction::require_single_function_call`.

## Client-side encoding (TypeScript)

`scripts/src/delegate.ts` wraps `@near-js/transactions`'
`signDelegateAction` + `encodeSignedDelegate` helpers:

```ts
import { buildAndSignFunctionCallIntent } from "./delegate.js";

const { base64 } = await buildAndSignFunctionCallIntent(
  {
    sender: "alice.testnet",
    receiver: "register.testnet",
    method: "set",
    args: { value: "42" },
    gas: 30n * 1_000_000_000_000n,
    nonce: someNonce,
    maxBlockHeight: block + 10_000n,
    publicKey: aliceKey.getPublicKey(),
  },
  aliceKey,
);

// base64 is ready for gate.submit_intent({ signed_delegate: base64 }).
```

The TypeScript side delegates encoding and signing to near-api-js's
official helpers, so wire-format drift with the Rust side is caught
immediately: any discrepancy would cause `env::ed25519_verify` to
fail, the submit_intent tx would panic, and our test harness would
log "signature verification failed".

## Cross-language round-trip test

- **Rust side** (`nep366::tests::delegate_round_trip` +
  `signed_delegate_round_trip`): serializes a DelegateAction, parses
  it back, asserts equality. Exercises borsh-encode → borsh-decode
  on all fields.
- **Rust side** (`verify_accepts_valid_signature`): signs with
  `ed25519-dalek`, verifies via `env::ed25519_verify`. Exercises the
  full signing-scheme flow inside the contract.
- **TypeScript side** (`test/delegate.test.ts`): produces base64 via
  the production helpers, decodes back, verifies signature tag and
  size. Exercises the TypeScript-side encoders.
- **End-to-end** (live on testnet via `sequential submit`): TS
  encodes, Rust decodes + verifies. Any wire-format drift fails
  here with "signature verification failed".

## What to do if the spec changes

NEP-461 is still in proposal status. If the discriminant or borsh
layout changes upstream:

1. Update `NEP_366_DELEGATE_DISCRIMINANT` in
   `contracts/gate/src/nep366.rs`.
2. Update the `DelegateAction` / `SignedDelegateAction` field
   layout to match the new spec.
3. Re-run `cargo test -p gate` — the round-trip tests will catch
   any internal drift.
4. The TypeScript side updates automatically when `@near-js/transactions`
   is bumped to the new spec (update the dep pin in
   `scripts/package.json`).

An end-to-end submit on testnet is the final gate: if the TS-produced
bytes don't deserialize + verify on the Rust side, something drifted.
