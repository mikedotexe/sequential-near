import { KeyPair } from "near-api-js";
import {
  ACCOUNTS,
  GAS_DELEGATE_INNER_TGAS,
  GAS_SUBMIT_TGAS,
} from "../src/config.js";
import { readCredential } from "../src/accounts.js";
import {
  connectSender,
  extractTraceEvents,
  getBlockHeight,
  txStatus,
  viewCall,
} from "../src/rpc.js";
import { makeDirectSender } from "../src/directSender.js";
import { buildAndSignFunctionCallIntent } from "../src/delegate.js";

async function main() {
  const aliceKey = readCredential(ACCOUNTS.alice);
  const near = await connectSender();
  const relayer = await makeDirectSender(near, ACCOUNTS.relayer);

  const nonce = BigInt(Date.now());
  const maxBlockHeight = (await getBlockHeight()) + 10_000n;
  const { base64 } = await buildAndSignFunctionCallIntent(
    {
      sender: ACCOUNTS.alice,
      receiver: ACCOUNTS.register,
      method: "set",
      args: { value: "77" },
      gas: BigInt(GAS_DELEGATE_INNER_TGAS) * 1_000_000_000_000n,
      nonce,
      maxBlockHeight,
      publicKey: aliceKey.getPublicKey(),
    },
    aliceKey,
  );
  const txHash = await relayer.broadcastFunctionCall(
    ACCOUNTS.gate,
    "submit_intent",
    { signed_delegate: base64 },
    GAS_SUBMIT_TGAS,
    0n,
  );
  console.log(`submit_tx = ${txHash}`);
  console.log(`nonce = ${nonce}`);

  // Wait for executed_optimistic
  const status = await txStatus(txHash, ACCOUNTS.relayer, "EXECUTED_OPTIMISTIC");
  console.log("--- submit trace ---");
  for (const ev of extractTraceEvents(status)) console.log(JSON.stringify(ev));
  console.log("--- submit receipts ---");
  for (const r of status.receipts_outcome) {
    const kind = "Failure" in r.outcome.status
      ? `FAIL: ${JSON.stringify(r.outcome.status.Failure)}`
      : "SuccessValue" in r.outcome.status
        ? `OK val=${r.outcome.status.SuccessValue}`
        : `CHN -> ${"SuccessReceiptId" in r.outcome.status ? r.outcome.status.SuccessReceiptId : "?"}`;
    console.log(`  ${r.id}: ${kind}`);
  }

  // Now inspect pending list
  const pending = await viewCall<string[]>(ACCOUNTS.gate, "list_pending", {});
  console.log(`pending after submit: ${JSON.stringify(pending)}`);
  const stats = await viewCall<[string, string, string, string, string]>(ACCOUNTS.gate, "stats", {});
  console.log(`stats after submit: ${JSON.stringify(stats)}`);
}
main().catch((e) => { console.error(e); process.exit(1); });
