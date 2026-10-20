# geo-lookup

Looks up place names through a slow remote geocoder, with a cache in front of it.

## `LruCache` (src/cache.js)

```js
const cache = new LruCache({ max: 100, ttlMs: 60_000, now: Date.now });
```

- `max` (required, at least 1): how many entries it holds. Setting a new key when it is
  full evicts the least recently used entry.
- `ttlMs` (default `Infinity`): an entry expires once `now() - <when it was set>` is
  `ttlMs` or more. An expired entry behaves exactly as if it had been deleted.
- `now` (default `Date.now`): the clock, in milliseconds; tests pass their own.

Methods:

- `get(key)`: the value, or `undefined`. A hit makes the entry the most recently used.
- `set(key, value)`: adds or replaces the entry, makes it the most recently used and
  restarts its time to live. Returns the cache.
- `has(key)`: whether there is an unexpired entry. It does **not** count as a use.
- `delete(key)`: removes the entry; returns whether there was one.
- `clear()`: removes everything (the stats stay).
- `size`: the number of unexpired entries.
- `keys()`: the unexpired keys as an array, least recently used first.
- `stats()`: `{ hits, misses, evictions }`. Every `get` is a hit or a miss (an expired
  entry is a miss). `evictions` counts only entries pushed out by `max`, not expired or
  deleted ones.

## `memoize` (src/fetcher.js)

`memoize(fn, cache)` wraps an async `fn(key)` so each key is fetched once while it stays
in the cache.
