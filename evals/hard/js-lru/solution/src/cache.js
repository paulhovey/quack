// A least-recently-used cache with a time to live; see README.md for what it must do.
// A Map keeps insertion order, so re-inserting an entry makes it the most recent.

export class LruCache {
  constructor({ max, ttlMs = Infinity, now = Date.now } = {}) {
    if (!Number.isInteger(max) || max < 1) {
      throw new RangeError("max must be an integer of at least 1");
    }
    this.max = max;
    this.ttlMs = ttlMs;
    this.now = now;
    this.map = new Map();
    this.counts = { hits: 0, misses: 0, evictions: 0 };
  }

  #live(key) {
    const entry = this.map.get(key);
    if (entry === undefined) {
      return undefined;
    }
    if (this.now() - entry.at >= this.ttlMs) {
      this.map.delete(key);
      return undefined;
    }
    return entry;
  }

  #purge() {
    for (const key of [...this.map.keys()]) {
      this.#live(key);
    }
  }

  get(key) {
    const entry = this.#live(key);
    if (entry === undefined) {
      this.counts.misses += 1;
      return undefined;
    }
    this.counts.hits += 1;
    this.map.delete(key);
    this.map.set(key, entry);
    return entry.value;
  }

  set(key, value) {
    this.map.delete(key);
    this.#purge();
    while (this.map.size >= this.max) {
      this.map.delete(this.map.keys().next().value);
      this.counts.evictions += 1;
    }
    this.map.set(key, { value, at: this.now() });
    return this;
  }

  has(key) {
    return this.#live(key) !== undefined;
  }

  delete(key) {
    return this.#live(key) !== undefined && this.map.delete(key);
  }

  clear() {
    this.map.clear();
  }

  get size() {
    this.#purge();
    return this.map.size;
  }

  keys() {
    this.#purge();
    return [...this.map.keys()];
  }

  stats() {
    return { ...this.counts };
  }
}
