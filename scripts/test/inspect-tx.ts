import { txStatus, extractTraceEvents } from "../src/rpc.js";
import { ACCOUNTS } from "../src/config.js";

async function main() {
  const txHash = process.argv[2];
  const sender = process.argv[3] ?? ACCOUNTS.relayer;
  if (!txHash) throw new Error("usage: inspect-tx.ts <tx-hash> [sender]");
  const status = await txStatus(txHash, sender, "FINAL");
  console.log("--- trace events ---");
  for (const ev of extractTraceEvents(status)) {
    console.log(JSON.stringify(ev));
  }
  console.log("--- receipt statuses ---");
  for (const r of status.receipts_outcome) {
    if ("Failure" in r.outcome.status) {
      console.log(`FAIL ${r.id}: ${JSON.stringify(r.outcome.status.Failure)}`);
    } else if ("SuccessValue" in r.outcome.status) {
      console.log(`OK   ${r.id}: value=${r.outcome.status.SuccessValue}`);
    } else if ("SuccessReceiptId" in r.outcome.status) {
      console.log(`CHN  ${r.id}: -> ${r.outcome.status.SuccessReceiptId}`);
    }
  }
}
main().catch((e) => { console.error(e); process.exit(1); });
