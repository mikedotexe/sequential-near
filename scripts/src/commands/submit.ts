import { mkdirSync, writeFileSync } from "node:fs";
import { join } from "node:path";
import { KeyPair } from "near-api-js";

import {
  ACCOUNTS,
  GAS_DELEGATE_INNER_TGAS,
  GAS_RESUME_TGAS,
  GAS_SUBMIT_TGAS,
  NEAR_NETWORK,
  RUNS_DIR,
} from "../config.js";
import { assertMasterCredentialPresent, readCredential } from "../accounts.js";
import {
  assertChainIdMatches,
  connectSender,
  extractTraceEvents,
  getBlockHeight,
  txStatus,
  viewCall,
  type TxStatusResult,
} from "../rpc.js";
import { makeDirectSender, type DirectSender } from "../directSender.js";
import {
  buildAndSignFunctionCallIntent,
  buildFunctionCallDelegate,
  signDelegate,
} from "../delegate.js";

export type SubmitVariant =
  | "claim"
  | "reject"
  | "timeout"
  | "bad-sig"
  | "expired"
  | "replay";

export type SubmitTarget = "register" | "ft-shim";

export interface SubmitOpts {
  variant: SubmitVariant;
  target: SubmitTarget;
}

interface RunContext {
  variant: SubmitVariant;
  target: SubmitTarget;
  aliceKey: KeyPair;
  alicePubKey: ReturnType<KeyPair["getPublicKey"]>;
  relayerSender: DirectSender;
  approverSender: DirectSender;
  runTimestamp: string;
  runDir: string;
}

async function buildContext(opts: SubmitOpts): Promise<RunContext> {
  const aliceKey = readCredential(ACCOUNTS.alice);
  const alicePubKey = aliceKey.getPublicKey();
  const near = await connectSender();
  const relayerSender = await makeDirectSender(near, ACCOUNTS.relayer);
  const approverSender = await makeDirectSender(near, ACCOUNTS.approver);
  const runTimestamp = new Date().toISOString().replace(/[:.]/g, "-");
  const runDir = join(RUNS_DIR, runTimestamp, `submit-${opts.variant}-${opts.target}`);
  mkdirSync(runDir, { recursive: true });
  return {
    variant: opts.variant,
    target: opts.target,
    aliceKey,
    alicePubKey,
    relayerSender,
    approverSender,
    runTimestamp,
    runDir,
  };
}

function targetMethodAndArgs(
  target: SubmitTarget,
): { receiver: string; method: string; args: Record<string, unknown> } {
  if (target === "register") {
    return {
      receiver: ACCOUNTS.register,
      method: "set",
      args: { value: String(11 + Math.floor(Math.random() * 89)) },
    };
  }
  return {
    receiver: ACCOUNTS.ftShim,
    method: "transfer",
    args: { receiver_id: ACCOUNTS.alice, amount: "1000" },
  };
}

function extractIntentId(status: TxStatusResult): bigint | null {
  for (const ev of extractTraceEvents(status)) {
    if (ev.ev === "intent_submitted" && typeof ev.id === "number") {
      return BigInt(ev.id);
    }
  }
  return null;
}

function extractFailureReason(status: TxStatusResult): string | null {
  if ("Failure" in status.status) {
    return JSON.stringify(status.status.Failure);
  }
  for (const r of status.receipts_outcome) {
    if ("Failure" in r.outcome.status) {
      return JSON.stringify(r.outcome.status.Failure);
    }
  }
  return null;
}

async function readTargetState(target: SubmitTarget): Promise<Record<string, unknown>> {
  if (target === "register") {
    const [current, log, setCount] = await viewCall<[string, string[], number]>(
      ACCOUNTS.register,
      "get",
      {},
    );
    return { current, log, set_count: setCount };
  }
  const balance = await viewCall<string>(ACCOUNTS.ftShim, "balance_of", {
    account_id: ACCOUNTS.alice,
  });
  const log = await viewCall<Array<[string, string, string]>>(
    ACCOUNTS.ftShim,
    "get_transfer_log",
    {},
  );
  return { alice_balance: balance, transfer_log: log };
}

/// Submit a valid intent and wait for the submit tx to execute
/// optimistically so we can read the `intent_submitted` trace. If the
/// submit itself fails (bad-sig / expired / replay), returns a null
/// intentId + the failure reason from the tx status.
async function submitAndObserve(
  ctx: RunContext,
  base64: string,
): Promise<{
  txHash: string;
  intentId: bigint | null;
  failureReason: string | null;
  status: TxStatusResult;
}> {
  const txHash = await ctx.relayerSender.broadcastFunctionCall(
    ACCOUNTS.gate,
    "submit_intent",
    { signed_delegate: base64 },
    GAS_SUBMIT_TGAS,
    0n,
  );
  const status = await txStatus(txHash, ACCOUNTS.relayer, "EXECUTED_OPTIMISTIC");
  const failureReason = extractFailureReason(status);
  const intentId = failureReason ? null : extractIntentId(status);
  return { txHash, intentId, failureReason, status };
}

async function buildSignedBase64(
  ctx: RunContext,
  nonce: bigint,
  maxBlockHeight: bigint,
): Promise<string> {
  const { receiver, method, args } = targetMethodAndArgs(ctx.target);
  const { base64 } = await buildAndSignFunctionCallIntent(
    {
      sender: ACCOUNTS.alice,
      receiver,
      method,
      args,
      gas: BigInt(GAS_DELEGATE_INNER_TGAS) * 1_000_000_000_000n,
      nonce,
      maxBlockHeight,
      publicKey: ctx.alicePubKey,
    },
    ctx.aliceKey,
  );
  return base64;
}

async function resumeIntent(
  ctx: RunContext,
  intentId: bigint,
  approve: boolean,
): Promise<string> {
  return ctx.approverSender.broadcastFunctionCall(
    ACCOUNTS.gate,
    "resume_intent",
    { intent_id: intentId.toString(), approve },
    GAS_RESUME_TGAS,
    0n,
  );
}

function writeRecord(ctx: RunContext, record: Record<string, unknown>): void {
  const path = join(ctx.runDir, "record.json");
  writeFileSync(path, JSON.stringify(record, null, 2));
  console.log(`[submit:${ctx.variant}:${ctx.target}] record ${path}`);
}

// ---------- variants ----------

async function runClaim(ctx: RunContext): Promise<void> {
  const pre = await readTargetState(ctx.target);
  const nonce = BigInt(Date.now());
  const maxBlockHeight = (await getBlockHeight()) + 10_000n;
  const base64 = await buildSignedBase64(ctx, nonce, maxBlockHeight);
  const { txHash: submitHash, intentId, failureReason } = await submitAndObserve(ctx, base64);
  if (failureReason) throw new Error(`submit failed: ${failureReason}`);
  if (intentId === null) throw new Error("could not extract intent_id from submit trace");
  const resumeHash = await resumeIntent(ctx, intentId, true);
  await new Promise((r) => setTimeout(r, 6_000));
  const post = await readTargetState(ctx.target);
  writeRecord(ctx, {
    variant: "claim",
    target: ctx.target,
    network: NEAR_NETWORK,
    intent_id: intentId.toString(),
    submit_tx: submitHash,
    resume_tx: resumeHash,
    state: { pre, post },
  });
}

async function runReject(ctx: RunContext): Promise<void> {
  const pre = await readTargetState(ctx.target);
  const nonce = BigInt(Date.now());
  const maxBlockHeight = (await getBlockHeight()) + 10_000n;
  const base64 = await buildSignedBase64(ctx, nonce, maxBlockHeight);
  const { txHash: submitHash, intentId, failureReason } = await submitAndObserve(ctx, base64);
  if (failureReason) throw new Error(`submit failed: ${failureReason}`);
  if (intentId === null) throw new Error("could not extract intent_id from submit trace");
  const resumeHash = await resumeIntent(ctx, intentId, false);
  await new Promise((r) => setTimeout(r, 4_000));
  const post = await readTargetState(ctx.target);
  writeRecord(ctx, {
    variant: "reject",
    target: ctx.target,
    network: NEAR_NETWORK,
    intent_id: intentId.toString(),
    submit_tx: submitHash,
    resume_tx: resumeHash,
    state: { pre, post, unchanged: JSON.stringify(pre) === JSON.stringify(post) },
  });
}

async function runTimeout(ctx: RunContext): Promise<void> {
  const pre = await readTargetState(ctx.target);
  const nonce = BigInt(Date.now());
  const maxBlockHeight = (await getBlockHeight()) + 10_000n;
  const base64 = await buildSignedBase64(ctx, nonce, maxBlockHeight);
  const { txHash: submitHash, intentId, failureReason } = await submitAndObserve(ctx, base64);
  if (failureReason) throw new Error(`submit failed: ${failureReason}`);
  if (intentId === null) throw new Error("could not extract intent_id from submit trace");
  console.log(
    `[submit:timeout:${ctx.target}] intent ${intentId} submitted, waiting ~210 blocks (~3.5 min)…`,
  );
  const startBlock = await getBlockHeight();
  while ((await getBlockHeight()) < startBlock + 210n) {
    await new Promise((r) => setTimeout(r, 5_000));
  }
  const finalStatus = await txStatus(submitHash, ACCOUNTS.relayer, "FINAL");
  const traceEvents = extractTraceEvents(finalStatus);
  const post = await readTargetState(ctx.target);
  writeRecord(ctx, {
    variant: "timeout",
    target: ctx.target,
    network: NEAR_NETWORK,
    intent_id: intentId.toString(),
    submit_tx: submitHash,
    trace_events: traceEvents,
    state: { pre, post, unchanged: JSON.stringify(pre) === JSON.stringify(post) },
  });
}

async function runBadSig(ctx: RunContext): Promise<void> {
  const nonce = BigInt(Date.now());
  const maxBlockHeight = (await getBlockHeight()) + 10_000n;
  const { receiver, method, args } = targetMethodAndArgs(ctx.target);
  const delegate = buildFunctionCallDelegate({
    sender: ACCOUNTS.alice,
    receiver,
    method,
    args,
    gas: BigInt(GAS_DELEGATE_INNER_TGAS) * 1_000_000_000_000n,
    nonce,
    maxBlockHeight,
    publicKey: ctx.alicePubKey,
  });
  const { encoded } = await signDelegate(delegate, ctx.aliceKey);
  const tampered = new Uint8Array(encoded);
  const lastIdx = tampered.length - 1;
  tampered[lastIdx] = (tampered[lastIdx] ?? 0) ^ 0x01;
  const base64 = Buffer.from(tampered).toString("base64");
  const { txHash, failureReason } = await submitAndObserve(ctx, base64);
  writeRecord(ctx, {
    variant: "bad-sig",
    target: ctx.target,
    network: NEAR_NETWORK,
    submit_tx: txHash,
    expected: "signature verification failed",
    actual_error: failureReason ?? "unexpected-success",
    rejected: failureReason !== null && /signature verification failed/.test(failureReason),
  });
}

async function runExpired(ctx: RunContext): Promise<void> {
  const nonce = BigInt(Date.now());
  const current = await getBlockHeight();
  const expiredMax = current - 1n;
  const { receiver, method, args } = targetMethodAndArgs(ctx.target);
  const { base64 } = await buildAndSignFunctionCallIntent(
    {
      sender: ACCOUNTS.alice,
      receiver,
      method,
      args,
      gas: BigInt(GAS_DELEGATE_INNER_TGAS) * 1_000_000_000_000n,
      nonce,
      maxBlockHeight: expiredMax,
      publicKey: ctx.alicePubKey,
    },
    ctx.aliceKey,
  );
  const { txHash, failureReason } = await submitAndObserve(ctx, base64);
  writeRecord(ctx, {
    variant: "expired",
    target: ctx.target,
    network: NEAR_NETWORK,
    submit_tx: txHash,
    expected: "intent expired",
    actual_error: failureReason ?? "unexpected-success",
    rejected: failureReason !== null && /expired/.test(failureReason),
  });
}

async function runReplay(ctx: RunContext): Promise<void> {
  const nonce = BigInt(Date.now());
  const maxBlockHeight = (await getBlockHeight()) + 10_000n;
  const base64 = await buildSignedBase64(ctx, nonce, maxBlockHeight);

  const first = await submitAndObserve(ctx, base64);
  if (first.failureReason) throw new Error(`first submit failed: ${first.failureReason}`);
  if (first.intentId !== null) {
    await resumeIntent(ctx, first.intentId, true);
    await new Promise((r) => setTimeout(r, 4_000));
  }

  const second = await submitAndObserve(ctx, base64);
  writeRecord(ctx, {
    variant: "replay",
    target: ctx.target,
    network: NEAR_NETWORK,
    first_submit_tx: first.txHash,
    first_intent_id: first.intentId?.toString() ?? null,
    second_submit_tx: second.txHash,
    expected: "replay rejected",
    actual_error: second.failureReason ?? "unexpected-success",
    rejected: second.failureReason !== null && /replay|nonce/.test(second.failureReason),
  });
}

export async function cmdSubmit(opts: SubmitOpts): Promise<void> {
  assertMasterCredentialPresent();
  await assertChainIdMatches();
  const ctx = await buildContext(opts);
  console.log(
    `[submit:${opts.variant}:${opts.target}] network=${NEAR_NETWORK} run=${ctx.runTimestamp}`,
  );
  switch (opts.variant) {
    case "claim":
      await runClaim(ctx);
      return;
    case "reject":
      await runReject(ctx);
      return;
    case "timeout":
      await runTimeout(ctx);
      return;
    case "bad-sig":
      await runBadSig(ctx);
      return;
    case "expired":
      await runExpired(ctx);
      return;
    case "replay":
      await runReplay(ctx);
      return;
  }
}
