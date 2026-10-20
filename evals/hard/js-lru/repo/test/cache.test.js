import assert from "node:assert/strict";
import { test } from "node:test";

import { LruCache } from "../src/cache.js";
import { memoize } from "../src/fetcher.js";

test("evicts the least recently used", () => {
  const c = new LruCache({ max: 2 });
  c.set("a", 1).set("b", 2);
  c.get("a");
  c.set("c", 3);
  assert.deepEqual(c.keys(), ["a", "c"]);
  assert.equal(c.get("b"), undefined);
});

test("entries expire", () => {
  let t = 0;
  const c = new LruCache({ max: 5, ttlMs: 100, now: () => t });
  c.set("a", 1);
  t = 99;
  assert.equal(c.get("a"), 1);
  t = 100;
  assert.equal(c.get("a"), undefined);
  assert.equal(c.size, 0);
});

test("stats", () => {
  const c = new LruCache({ max: 1 });
  c.set("a", 1);
  c.get("a");
  c.get("b");
  c.set("b", 2);
  assert.deepEqual(c.stats(), { hits: 1, misses: 1, evictions: 1 });
});

test("memoize fetches each key once", async () => {
  let calls = 0;
  const f = memoize(async (k) => {
    calls += 1;
    return k.toUpperCase();
  }, new LruCache({ max: 10 }));
  assert.equal(await f("x"), "X");
  assert.equal(await f("x"), "X");
  assert.equal(calls, 1);
});
