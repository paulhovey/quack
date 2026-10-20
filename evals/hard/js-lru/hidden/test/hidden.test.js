import assert from "node:assert/strict";
import { test } from "node:test";

import { LruCache } from "../src/cache.js";

test("has is not a use", () => {
  const c = new LruCache({ max: 2 });
  c.set("a", 1).set("b", 2);
  assert.equal(c.has("a"), true);
  c.set("c", 3);
  assert.deepEqual(c.keys(), ["b", "c"]);
  assert.deepEqual(c.stats(), { hits: 0, misses: 0, evictions: 1 });
});

test("replacing a key is not an eviction and restarts its time", () => {
  let t = 0;
  const c = new LruCache({ max: 2, ttlMs: 10, now: () => t });
  c.set("a", 1).set("b", 2);
  t = 5;
  c.set("a", 9);
  assert.deepEqual(c.keys(), ["b", "a"]);
  t = 12;
  assert.equal(c.get("b"), undefined);
  assert.equal(c.get("a"), 9);
  assert.deepEqual(c.stats(), { hits: 1, misses: 1, evictions: 0 });
});

test("expired entries make room without counting as evictions", () => {
  let t = 0;
  const c = new LruCache({ max: 2, ttlMs: 10, now: () => t });
  c.set("a", 1).set("b", 2);
  t = 10;
  c.set("c", 3).set("d", 4);
  assert.deepEqual(c.keys(), ["c", "d"]);
  assert.equal(c.stats().evictions, 0);
  assert.equal(c.has("a"), false);
});

test("delete, clear and size", () => {
  let t = 0;
  const c = new LruCache({ max: 3, ttlMs: 10, now: () => t });
  c.set("a", 1).set("b", 2).set("c", 3);
  assert.equal(c.delete("b"), true);
  assert.equal(c.delete("b"), false);
  assert.equal(c.size, 2);
  t = 10;
  assert.equal(c.delete("a"), false);
  assert.equal(c.size, 0);
  c.set("x", 1);
  c.get("x");
  c.clear();
  assert.equal(c.size, 0);
  assert.equal(c.stats().hits, 1);
});

test("falsy values are values", () => {
  const c = new LruCache({ max: 2 });
  c.set("zero", 0).set("empty", "");
  assert.equal(c.get("zero"), 0);
  assert.equal(c.get("empty"), "");
  assert.equal(c.stats().hits, 2);
});

test("no ttl means no expiry", () => {
  let t = 0;
  const c = new LruCache({ max: 1, now: () => t });
  c.set("a", 1);
  t = 1e15;
  assert.equal(c.get("a"), 1);
});
