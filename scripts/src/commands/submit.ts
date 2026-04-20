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
  runTimestamp: string;
  runDir: string;
}

async function buildContext(opts: SubmitOpts): Promise<RunContext> {
  const aliceKey = readCredential(ACCOUNTS.alice);
  const alicePubKey = aliceKey.getPublicKey();
  const runTimestamp = new Date().toISOString().replace(/[:.]/g, "-");
  const runDir = join(RUNS_DIR, runTimestamp, `submit-${opts.variant}-${opts.target}`);
  mkdirSync(runDir, { recursive: true });
  return {
    variant: opts.variant,
    target: opts.target,
    aliceKey,
    alicePubKey,
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

async function relayerCall(
  methodName: string,
  args: Record<string, unknown>,
  gasTgas: number,
): Promise<string> {
  const near = await connectSender();
  const relayer = await near.account(ACCOUNTS.relayer);
  const outcome = await relayer.functionCall({
    contractId: ACCOUNTS.gate,
    methodName,
    args,
    gas: BigInt(gasTgas) * 1_000_000_000_000n,
  });
  return outcome.transaction.hash;
}

async function approverCall(
  methodName: string,
  args: Record<string, unknown>,
  gasTgas: number,
): Promise<string> {
  const near = await connectSender();
  const approver = await near.account(ACCOUNTS.approver);
  const outcome = await approver.functionCall({
    contractId: ACCOUNTS.gate,
    methodName,
    args,
    gas: BigInt(gasTgas) * 1_000_000_000_000n,
  });
  return outcome.transaction.hash;
}

async function readTargetState(
  target: SubmitTarget,
): Promise<Record<string, unknown>> {
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

async function submitValidIntent(
  ctx: RunContext,
  nonce: bigint,
  maxBlockHeight: bigint,
): Promise<{ txHash: string; intentId: bigint | null; submitStatus: TxStatusResult }> {
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
  const txHash = await relayerCall("submit_intent", { signed_delegate: base64 }, GAS_SUBMIT_TGAS);
  const submitStatus = await txStatus(txHash, ACCOUNTS.relayer, "EXECUTED_OPTIMISTIC");
  const intentId = extractIntentId(submitStatus);
  return { txHash, intentId, submitStatus };
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
  const { txHash: submitHash, intentId } = await submitValidIntent(ctx, nonce, maxBlockHeight);
  if (intentId === null) throw new Error("could not extract intent_id from submit trace");
  const resumeHash = await approverCall(
    "resume_intent",
    { intent_id: intentId.toString(), approve: true },
    GAS_RESUME_TGAS,
  );
  // Give the dispatched receipt a few seconds to land + commit.
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
  const { txHash: submitHash, intentId } = await submitValidIntent(ctx, nonce, maxBlockHeight);
  if (intentId === null) throw new Error("could not extract intent_id from submit trace");
  const resumeHash = await approverCall(
    "resume_intent",
    { intent_id: intentId.toString(), approve: false },
    GAS_RESUME_TGAS,
  );
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
  const { txHash: submitHash, intentId } = await submitValidIntent(ctx, nonce, maxBlockHeight);
  if (intentId === null) throw new Error("could not extract intent_id from submit trace");
  console.log(
    `[submit:timeout:${ctx.target}] intent ${intentId} submitted, waiting ~210 blocks (~3.5 min)…`,
  );
  // NEP-519 budget is 202 blocks; wait a bit past that.
  const startBlock = await getBlockHeight();
  while ((await getBlockHeight()) < startBlock + 210n) {
    await new Promise((r) => setTimeout(r, 5_000));
  }
  // After 210 blocks, poll the submit tx for the final (post-callback) trace.
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
  // Tamper with the last byte (inside the signature bytes).
  const tampered = new Uint8Array(encoded);
  const lastIdx = tampered.length - 1;
  tampered[lastIdx] = (tampered[lastIdx] ?? 0) ^ 0x01;
  const base64 = Buffer.from(tampered).toString("base64");
  let errorMessage = "unexpected-success";
  try {
    await relayerCall("submit_intent", { signed_delegate: base64 }, GAS_SUBMIT_TGAS);
  } catch (err) {
    errorMessage = (err as Error).message ?? String(err);
  }
  writeRecord(ctx, {
    variant: "bad-sig",
    target: ctx.target,
    network: NEAR_NETWORK,
    expected: "signature verification failed",
    actual_error: errorMessage,
    rejected: /signature verification failed/.test(errorMessage),
  });
}

async function runExpired(ctx: RunContext): Promise<void> {
  const nonce = BigInt(Date.now());
  const current = await getBlockHeight();
  const expiredMax = current - 1n; // already expired
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
  let errorMessage = "unexpected-success";
  try {
    await relayerCall("submit_intent", { signed_delegate: base64 }, GAS_SUBMIT_TGAS);
  } catch (err) {
    errorMessage = (err as Error).message ?? String(err);
  }
  writeRecord(ctx, {
    variant: "expired",
    target: ctx.target,
    network: NEAR_NETWORK,
    expected: "intent expired",
    actual_error: errorMessage,
    rejected: /expired/.test(errorMessage),
  });
}

async function runReplay(ctx: RunContext): Promise<void> {
  const nonce = BigInt(Date.now());
  const maxBlockHeight = (await getBlockHeight()) + 10_000n;
  const { txHash: firstHash, intentId: firstId } = await submitValidIntent(ctx, nonce, maxBlockHeight);
  // Approve + wait so the replay test isn't affected by pending state.
  if (firstId !== null) {
    await approverCall(
      "resume_intent",
      { intent_id: firstId.toString(), approve: true },
      GAS_RESUME_TGAS,
    );
    await new Promise((r) => setTimeout(r, 4_000));
  }
  // Second submit with SAME nonce — should be rejected.
  let errorMessage = "unexpected-success";
  try {
    await submitValidIntent(ctx, nonce, maxBlockHeight);
  } catch (err) {
    errorMessage = (err as Error).message ?? String(err);
  }
  writeRecord(ctx, {
    variant: "replay",
    target: ctx.target,
    network: NEAR_NETWORK,
    first_submit_tx: firstHash,
    first_intent_id: firstId?.toString() ?? null,
    expected: "replay rejected",
    actual_error: errorMessage,
    rejected: /replay|nonce/.test(errorMessage),
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
