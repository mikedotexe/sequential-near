#!/usr/bin/env tsx
import { cmdClean } from "./commands/clean.js";
import { cmdDeploy } from "./commands/deploy.js";

const HELP = `sequential — signed-intent sequencer workflow

Usage:
  tsx src/index.ts <command> [flags]

Commands:
  deploy                       create accounts + deploy contracts
  clean                        destroy all accounts (routes balance to master)

Flags:
  --i-know-this-is-testnet     required for \`clean\` on testnet
  --i-know-this-is-mainnet     required for \`deploy\` and \`clean\` on mainnet

Env:
  NEAR_NETWORK=testnet|mainnet  default: testnet
  MASTER_ACCOUNT_ID             default: mike.{testnet,near}
  FASTNEAR_API_KEY              optional; raises rate limits
`;

async function main(): Promise<void> {
  const [, , cmd] = process.argv;
  switch (cmd) {
    case "deploy":
      await cmdDeploy();
      break;
    case "clean":
      await cmdClean();
      break;
    case undefined:
    case "help":
    case "--help":
    case "-h":
      process.stdout.write(HELP);
      return;
    default:
      process.stderr.write(`unknown command: ${cmd}\n\n${HELP}`);
      process.exit(1);
  }
}

main().catch((err) => {
  process.stderr.write(`error: ${err.message ?? err}\n`);
  if (process.env.DEBUG) {
    process.stderr.write(`${err.stack ?? ""}\n`);
  }
  process.exit(1);
});
