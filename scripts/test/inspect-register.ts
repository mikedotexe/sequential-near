import { viewCall } from "../src/rpc.js";
import { ACCOUNTS } from "../src/config.js";

async function main() {
  const state = await viewCall<[string, string[], number]>(
    ACCOUNTS.register,
    "get",
    {},
  );
  console.log(JSON.stringify(state));
}
main().catch((e) => { console.error(e); process.exit(1); });
