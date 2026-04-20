import { viewCall } from "../src/rpc.js";
import { ACCOUNTS } from "../src/config.js";

async function main() {
  const stats = await viewCall<[string, string, string, string, string]>(
    ACCOUNTS.gate,
    "stats",
    {},
  );
  const pending = await viewCall<string[]>(ACCOUNTS.gate, "list_pending", {});
  console.log(`stats: submitted=${stats[0]} dispatched=${stats[1]} rejected=${stats[2]} next_id=${stats[3]} batch_id=${stats[4]}`);
  console.log(`pending ids: ${JSON.stringify(pending)}`);
  for (const id of pending) {
    const view = await viewCall<Record<string, unknown>>(ACCOUNTS.gate, "get_pending", { intent_id: id });
    console.log(`pending[${id}]: ${JSON.stringify(view)}`);
  }
}
main().catch((e) => { console.error(e); process.exit(1); });
