import { ACCOUNTS, NEAR_NETWORK } from "../config.js";
import { assertMasterCredentialPresent, destroySubAccount } from "../accounts.js";
import { assertChainIdMatches } from "../rpc.js";

function refuseWithoutGate(): void {
  const flag =
    NEAR_NETWORK === "mainnet"
      ? "--i-know-this-is-mainnet"
      : "--i-know-this-is-testnet";
  const args = process.argv.slice(2);
  if (!args.includes(flag)) {
    throw new Error(
      `refusing to clean ${NEAR_NETWORK} without ${flag} flag — this deletes accounts`,
    );
  }
}

export async function cmdClean(): Promise<void> {
  refuseWithoutGate();
  assertMasterCredentialPresent();
  await assertChainIdMatches();

  console.log(`[clean] network=${NEAR_NETWORK}`);

  // Destroy in reverse of deploy order so child accounts go first
  // (minor; destroyAccount doesn't care about topology here).
  const roles: (keyof typeof ACCOUNTS)[] = [
    "ftShim",
    "register",
    "gate",
    "approver",
    "relayer",
    "alice",
  ];
  for (const role of roles) {
    const res = await destroySubAccount(ACCOUNTS[role]);
    console.log(`[clean] ${role}=${ACCOUNTS[role]} ${res}`);
  }

  console.log(`[clean] done`);
}
