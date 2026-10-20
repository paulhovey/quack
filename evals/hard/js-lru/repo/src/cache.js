// A least-recently-used cache with a time to live; see README.md for what it must do.
// So far it is only a Map.

export class LruCache {
  constructor({ max, ttlMs = Infinity, now = Date.now } = {}) {
    this.max = max;
    this.ttlMs = ttlMs;
    this.now = now;
    this.map = new Map();
  }

  get(key) {
    return this.map.get(key);
  }

  set(key, value) {
    this.map.set(key, value);
    return this;
  }
}
