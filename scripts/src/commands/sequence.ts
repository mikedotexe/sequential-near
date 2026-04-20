import { mkdirSync, writeFileSync } from "node:fs";
import { join } from "node:path";

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
import { buildAndSignFunctionCallIntent } from "../delegate.js";
import type { SubmitTarget } from "./submit.js";

export interface SequenceOpts {
  n: number;
  target: SubmitTarget;
  /// "identity", "random", a comma-separated list (e.g. "2,0,1"), or a
  /// pre-resolved number[].
  permutation: string | number[];
}

function parsePermutation(spec: string, n: number): number[] {
  if (spec === "identity") return Array.from({ length: n }, (_, i) => i);
  if (spec === "random") {
    const arr = Array.from({ length: n }, (_, i) => i);
    for (let i = arr.length - 1; i > 0; i--) {
      const j = Math.floor(Math.random() * (i + 1));
      [arr[i], arr[j]] = [arr[j]!, arr[i]!];
    }
    return arr;
  }
  const ints = spec.split(",").map((s) => parseInt(s.trim(), 10));
  if (ints.some((v) => Number.isNaN(v))) {
    throw new Error(`invalid permutation spec: ${spec}`);
  }
  if (ints.length !== n) {
    throw new Error(`permutation length ${ints.length} !== n ${n}`);
  }
  const sorted = [...ints].sort((a, b) => a - b);
  for (let i = 0; i < n; i++) {
    if (sorted[i] !== i) {
      throw new Error(
        `permutation must be a permutation of [0..${n - 1}], got ${JSON.stringify(ints)}`,
      );
    }
  }
  return ints;
}

export function resolvePermutation(p: SequenceOpts["permutation"], n: number): number[] {
  if (typeof p === "string") return parsePermutation(p, n);
  return parsePermutation(p.join(","), n);
}

function valuesForRegister(n: number): number[] {
  // Distinct ascending values so the expected log is obvious.
  return Array.from({ length: n }, (_, i) => 11 + i * 11);
}

function amountsForFtShim(n: number): bigint[] {
  // Distinct ascending amounts.
  return Array.from({ length: n }, (_, i) => BigInt(100 + i * 17));
}

function targetIntentArgs(
  target: SubmitTarget,
  index: number,
  values: number[],
  amounts: bigint[],
): { receiver: string; method: string; args: Record<string, unknown> } {
  if (target === "register") {
    return {
      receiver: ACCOUNTS.register,
      method: "set",
      args: { value: String(values[index]) },
    };
  }
  return {
    receiver: ACCOUNTS.ftShim,
    method: "transfer",
    args: { receiver_id: ACCOUNTS.alice, amount: amounts[index]!.toString() },
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

async function submitIntent(
  target: SubmitTarget,
  method: string,
  args: Record<string, unknown>,
  receiver: string,
  nonce: bigint,
  maxBlockHeight: bigint,
  aliceKey: ReturnType<typeof readCredential>,
  alicePubKey: ReturnType<typeof readCredential>["getPublicKey"] extends () => infer R ? R : never,
): Promise<{ txHash: string; intentId: bigint }> {
  const { base64 } = await buildAndSignFunctionCallIntent(
    {
      sender: ACCOUNTS.alice,
      receiver,
      method,
      args,
      gas: BigInt(GAS_DELEGATE_INNER_TGAS) * 1_000_000_000_000n,
      nonce,
      maxBlockHeight,
      publicKey: alicePubKey,
    },
    aliceKey,
  );
  const near = await connectSender();
  const relayer = await near.account(ACCOUNTS.relayer);
  const outcome = await relayer.functionCall({
    contractId: ACCOUNTS.gate,
    methodName: "submit_intent",
    args: { signed_delegate: base64 },
    gas: BigInt(GAS_SUBMIT_TGAS) * 1_000_000_000_000n,
  });
  const status = await txStatus(outcome.transaction.hash, ACCOUNTS.relayer, "EXECUTED_OPTIMISTIC");
  const intentId = extractIntentId(status);
  if (intentId === null) {
    throw new Error(`could not extract intent_id from submit tx ${outcome.transaction.hash}`);
  }
  return { txHash: outcome.transaction.hash, intentId };
}

async function readRegisterState(): Promise<{ current: string; log: string[]; set_count: number }> {
  const [current, log, setCount] = await viewCall<[string, string[], number]>(
    ACCOUNTS.register,
    "get",
    {},
  );
  return { current, log, set_count: setCount };
}

async function readFtShimState(): Promise<{
  alice_balance: string;
  transfer_log: Array<[string, string, string]>;
}> {
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

export async function cmdSequence(opts: SequenceOpts): Promise<void> {
  assertMasterCredentialPresent();
  await assertChainIdMatches();

  if (opts.n < 2) {
    throw new Error(`--n must be >= 2, got ${opts.n}`);
  }

  const permutation = resolvePermutation(opts.permutation, opts.n);
  const runTimestamp = new Date().toISOString().replace(/[:.]/g, "-");
  const runDir = join(RUNS_DIR, runTimestamp, `sequence-n${opts.n}-${opts.target}`);
  mkdirSync(runDir, { recursive: true });

  console.log(
    `[sequence] network=${NEAR_NETWORK} n=${opts.n} target=${opts.target} permutation=[${permutation.join(",")}]`,
  );

  const aliceKey = readCredential(ACCOUNTS.alice);
  const alicePubKey = aliceKey.getPublicKey();

  const values = valuesForRegister(opts.n);
  const amounts = amountsForFtShim(opts.n);

  const pre =
    opts.target === "register" ? await readRegisterState() : await readFtShimState();

  // 1) Submit N intents in submission order (0..n-1). Each gets a distinct
  //    nonce and a far-future max_block_height.
  const maxBlockHeight = (await getBlockHeight()) + 10_000n;
  const nonceBase = BigInt(Date.now());
  const submits: Array<{ txHash: string; intentId: bigint; submissionIndex: number }> = [];
  for (let i = 0; i < opts.n; i++) {
    const { receiver, method, args } = targetIntentArgs(opts.target, i, values, amounts);
    const res = await submitIntent(
      opts.target,
      method,
      args,
      receiver,
      nonceBase + BigInt(i),
      maxBlockHeight,
      aliceKey,
      alicePubKey,
    );
    submits.push({ ...res, submissionIndex: i });
    console.log(`[sequence] submitted idx=${i} intent_id=${res.intentId}`);
  }

  // 2) Build the batch in permutation order.
  const batchIds = permutation.map((sourceIdx) => submits[sourceIdx]!.intentId);

  // 3) Approver calls resume_batch_chained(ids).
  const near = await connectSender();
  const approver = await near.account(ACCOUNTS.approver);
  const batchOutcome = await approver.functionCall({
    contractId: ACCOUNTS.gate,
    methodName: "resume_batch_chained",
    args: { intent_ids: batchIds.map((id) => id.toString()) },
    gas: BigInt(GAS_RESUME_TGAS) * 1_000_000_000_000n,
  });
  console.log(`[sequence] batch resumed tx=${batchOutcome.transaction.hash}`);

  // 4) Wait for all chain steps to land. Each step is ~+3 blocks; on testnet
  //    blocks are ~1s. Wait n * 5s to be safe.
  const waitMs = Math.max(15_000, opts.n * 5_000);
  await new Promise((r) => setTimeout(r, waitMs));

  // 5) Read target state, compute expected from permutation, compare.
  const post =
    opts.target === "register" ? await readRegisterState() : await readFtShimState();

  let expected: Record<string, unknown>;
  let match: boolean;
  if (opts.target === "register") {
    const expectedLog = permutation.map((i) => String(values[i]));
    const expectedCurrent = expectedLog[expectedLog.length - 1]!;
    expected = { current: expectedCurrent, log: expectedLog };
    const postR = post as Awaited<ReturnType<typeof readRegisterState>>;
    // Compare against the N most recent entries in log (state may have prior
    // entries from earlier runs).
    const tail = postR.log.slice(postR.log.length - opts.n);
    match = postR.current === expectedCurrent && JSON.stringify(tail) === JSON.stringify(expectedLog);
  } else {
    const expectedAmounts = permutation.map((i) => amounts[i]!.toString());
    expected = { transfer_log_tail: expectedAmounts };
    const postF = post as Awaited<ReturnType<typeof readFtShimState>>;
    const tail = postF.transfer_log
      .slice(postF.transfer_log.length - opts.n)
      .map(([_from, _to, amount]) => amount);
    match = JSON.stringify(tail) === JSON.stringify(expectedAmounts);
  }

  const record = {
    target: opts.target,
    network: NEAR_NETWORK,
    n: opts.n,
    permutation,
    values: opts.target === "register" ? values : amounts.map((a) => a.toString()),
    submits: submits.map((s) => ({
      submission_index: s.submissionIndex,
      intent_id: s.intentId.toString(),
      tx: s.txHash,
    })),
    batch_tx: batchOutcome.transaction.hash,
    batch_ids: batchIds.map((id) => id.toString()),
    state: { pre, post },
    expected,
    match,
  };
  writeFileSync(join(runDir, "record.json"), JSON.stringify(record, null, 2));
  console.log(
    `[sequence] ${match ? "MATCH" : "MISMATCH"} record=${join(runDir, "record.json")}`,
  );
  if (!match) {
    process.exitCode = 2;
  }
}
