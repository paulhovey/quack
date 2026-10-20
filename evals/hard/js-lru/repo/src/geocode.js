import { memoize } from "./fetcher.js";

async function remote(name) {
  const res = await fetch(`https://geocoder.example.org/v1?q=${encodeURIComponent(name)}`);
  if (!res.ok) {
    throw new Error(`geocoder: ${res.status}`);
  }
  return res.json();
}

export const geocode = memoize(remote);
