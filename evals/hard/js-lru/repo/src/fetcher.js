import { LruCache } from "./cache.js";

export function memoize(fn, cache = new LruCache({ max: 500, ttlMs: 10 * 60_000 })) {
  return async (key) => {
    const hit = cache.get(key);
    if (hit !== undefined) {
      return hit;
    }
    const value = await fn(key);
    cache.set(key, value);
    return value;
  };
}
