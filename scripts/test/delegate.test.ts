import { KeyPair } from "near-api-js";

import { buildAndSignFunctionCallIntent, buildFunctionCallDelegate } from "../src/delegate.js";

// Lightweight standalone test runner. tsx invokes this file; assertions
// throw on failure, which tsx surfaces with exit code 1. Enough signal to
// gate CI without pulling in a test framework dep.

function assert(cond: unknown, msg: string): void {
  if (!cond) throw new Error(`assertion failed: ${msg}`);
}

async function main() {
  const kp = KeyPair.fromRandom("ed25519");
  const pk = kp.getPublicKey();

  // 1. Build + sign a delegate
  const { base64, hash } = await buildAndSignFunctionCallIntent(
    {
      sender: "alice.testnet",
      receiver: "register.testnet",
      method: "set",
      args: { value: "42" },
      nonce: 1n,
      maxBlockHeight: 123_456_789n,
      publicKey: pk,
    },
    kp,
  );

  assert(typeof base64 === "string" && base64.length > 0, "base64 produced");
  assert(hash.byteLength === 32, "hash is 32 bytes");

  // 2. base64 decodes back to borsh bytes of reasonable size
  const bytes = Buffer.from(base64, "base64");
  // sender(borsh) + receiver(borsh) + 1 action w/ FunctionCall + nonce + max_block + pk + sig
  // Minimum expected: ~170 bytes; empirical ~200+
  assert(bytes.length >= 150 && bytes.length <= 400, `wire bytes size ok: ${bytes.length}`);

  // 3. Last 65 bytes are the signature (tag + 64 ed25519 bytes)
  const sigTag = bytes[bytes.length - 65];
  assert(sigTag === 0, `signature tag is ed25519 (0), got ${sigTag}`);

  // 4. Different args → different bytes (non-trivial signing)
  const other = await buildAndSignFunctionCallIntent(
    {
      sender: "alice.testnet",
      receiver: "register.testnet",
      method: "set",
      args: { value: "99" },
      nonce: 1n,
      maxBlockHeight: 123_456_789n,
      publicKey: pk,
    },
    kp,
  );
  assert(other.base64 !== base64, "different args → different encoded bytes");

  // 5. Deterministic build (no random in pre-sign): same inputs → same delegate object
  const d1 = buildFunctionCallDelegate({
    sender: "alice.testnet",
    receiver: "register.testnet",
    method: "set",
    args: { value: "42" },
    nonce: 1n,
    maxBlockHeight: 123_456_789n,
    publicKey: pk,
  });
  const d2 = buildFunctionCallDelegate({
    sender: "alice.testnet",
    receiver: "register.testnet",
    method: "set",
    args: { value: "42" },
    nonce: 1n,
    maxBlockHeight: 123_456_789n,
    publicKey: pk,
  });
  assert(d1.senderId === d2.senderId, "deterministic senderId");
  assert(d1.nonce === d2.nonce, "deterministic nonce");

  console.log("[delegate.test] 5 assertions passed");
}

main().catch((err) => {
  console.error(err);
  process.exit(1);
});
