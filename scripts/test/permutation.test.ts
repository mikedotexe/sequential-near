import { resolvePermutation } from "../src/commands/sequence.js";

function assert(cond: unknown, msg: string): void {
  if (!cond) throw new Error(`assertion failed: ${msg}`);
}

function eq<T>(a: T[], b: T[]): boolean {
  return a.length === b.length && a.every((v, i) => v === b[i]);
}

function expectThrow(fn: () => unknown, pattern: RegExp): void {
  try {
    fn();
  } catch (err) {
    const msg = (err as Error).message ?? String(err);
    if (!pattern.test(msg)) {
      throw new Error(`expected error matching ${pattern}, got: ${msg}`);
    }
    return;
  }
  throw new Error(`expected throw matching ${pattern}, got no throw`);
}

function main(): void {
  // identity
  assert(eq(resolvePermutation("identity", 3), [0, 1, 2]), "identity n=3");
  assert(eq(resolvePermutation("identity", 5), [0, 1, 2, 3, 4]), "identity n=5");

  // csv
  assert(eq(resolvePermutation("2,0,1", 3), [2, 0, 1]), "csv simple");
  assert(eq(resolvePermutation("0,1,2,3,4", 5), [0, 1, 2, 3, 4]), "csv identity");
  assert(eq(resolvePermutation("4,3,2,1,0", 5), [4, 3, 2, 1, 0]), "csv reverse");

  // random produces a valid permutation
  for (let trial = 0; trial < 20; trial++) {
    const p = resolvePermutation("random", 5);
    const sorted = [...p].sort((a, b) => a - b);
    assert(eq(sorted, [0, 1, 2, 3, 4]), `random trial ${trial} is a permutation`);
  }

  // validation: rejects wrong length
  expectThrow(() => resolvePermutation("2,0", 3), /length/);
  // validation: rejects duplicate
  expectThrow(() => resolvePermutation("0,0,1", 3), /must be a permutation/);
  // validation: rejects out-of-range
  expectThrow(() => resolvePermutation("0,1,9", 3), /must be a permutation/);
  // validation: rejects non-numeric
  expectThrow(() => resolvePermutation("a,b,c", 3), /invalid permutation spec/);

  // array input
  assert(eq(resolvePermutation([1, 2, 0], 3), [1, 2, 0]), "array input");

  console.log("[permutation.test] all assertions passed");
}

main();
