import { actionCreators } from "@near-js/transactions";
import {
  buildDelegateAction,
  encodeSignedDelegate,
  signDelegateAction,
  type DelegateAction,
} from "@near-js/transactions";
import type { PublicKey } from "@near-js/crypto";
import type { KeyPair } from "near-api-js";

/// NEP-366 delegate builder + signer + encoder.
///
/// Produces a byte-for-byte NEAR-primitives-compatible
/// `SignedDelegateAction`, base64-encoded for the gate's
/// `submit_intent(signed_delegate_base64: String)`. Uses
/// `@near-js/transactions`' official encoders under the hood so
/// wire-format drift with the Rust side (contracts/gate/src/nep366.rs)
/// is caught by the gate's signature-verify step at submit time.
///
/// v0.1: only FunctionCall inner actions supported.

export type FunctionCallArgs = {
  sender: string;
  receiver: string;
  method: string;
  args: unknown; // JSON-serializable
  deposit?: bigint;
  gas?: bigint;
  nonce: bigint;
  maxBlockHeight: bigint;
  publicKey: PublicKey;
};

export function buildFunctionCallDelegate(opts: FunctionCallArgs): DelegateAction {
  const argsBytes = new TextEncoder().encode(JSON.stringify(opts.args));
  const fc = actionCreators.functionCall(
    opts.method,
    argsBytes,
    opts.gas ?? 30n * 1_000_000_000_000n,
    opts.deposit ?? 0n,
  );
  return buildDelegateAction({
    senderId: opts.sender,
    receiverId: opts.receiver,
    actions: [fc],
    nonce: opts.nonce,
    maxBlockHeight: opts.maxBlockHeight,
    publicKey: opts.publicKey,
  });
}

/// Sign a delegate with the sender's key. Returns the borsh-encoded
/// SignedDelegate bytes (wire-format-ready) plus the signed-message
/// hash (useful for debugging / cross-checking the gate's verify).
export async function signDelegate(
  delegate: DelegateAction,
  signerKey: KeyPair,
): Promise<{ encoded: Uint8Array; base64: string; hash: Uint8Array }> {
  const signer = {
    async sign(message: Uint8Array): Promise<Uint8Array> {
      return signerKey.sign(message).signature;
    },
  };
  const { signedDelegateAction, hash } = await signDelegateAction({
    delegateAction: delegate,
    signer,
  });
  const encoded = encodeSignedDelegate(signedDelegateAction);
  const base64 = Buffer.from(encoded).toString("base64");
  return { encoded, base64, hash };
}

/// Convenience one-shot: build + sign + encode in a single call for a
/// FunctionCall intent. Returns the base64 string ready for
/// `submit_intent(signed_delegate_base64)`.
export async function buildAndSignFunctionCallIntent(
  opts: FunctionCallArgs,
  signerKey: KeyPair,
): Promise<{ base64: string; hash: Uint8Array }> {
  const delegate = buildFunctionCallDelegate(opts);
  const signed = await signDelegate(delegate, signerKey);
  return { base64: signed.base64, hash: signed.hash };
}
