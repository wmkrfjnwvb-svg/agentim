// Encryption of the local store (docs/12 §2.1): AES-256-GCM over the
// content of each row, bound to where the row lives (conversation and
// key), so rows cannot be swapped or moved unnoticed. Ids, seqs and times
// stay plain for queries and eviction.
//
// Keys: in browsers a non-extractable WebCrypto key kept in IndexedDB
// (`browserKey`); on React Native and Node, 32 bytes the app keeps in the
// keychain / Keystore (`keyFromBytes`); or the app's own functions
// (`customCodec`, like Sendbird's localCacheEncryption).

import type { Codec } from './types.js';

const PREFIX = 'e1:';

function subtle(): SubtleCrypto {
  const s = globalThis.crypto?.subtle;
  if (!s) throw new Error('WebCrypto is not available: pass customCodec() with your own encryption');
  return s;
}

function toB64(b: Uint8Array): string {
  let s = '';
  for (let i = 0; i < b.length; i += 0x8000) s += String.fromCharCode(...b.subarray(i, i + 0x8000));
  return btoa(s);
}

function fromB64(s: string): Uint8Array<ArrayBuffer> {
  const bin = atob(s);
  const out = new Uint8Array(bin.length);
  for (let i = 0; i < bin.length; i++) out[i] = bin.charCodeAt(i);
  return out;
}

const enc = new TextEncoder();
const dec = new TextDecoder();

/** Content encrypted with AES-256-GCM under `key`. */
export function aesCodec(key: CryptoKey): Codec {
  return {
    async encode(value, aad) {
      const iv = globalThis.crypto.getRandomValues(new Uint8Array(12));
      const ct = new Uint8Array(
        await subtle().encrypt({ name: 'AES-GCM', iv, additionalData: enc.encode(aad) }, key, enc.encode(JSON.stringify(value))),
      );
      const out = new Uint8Array(12 + ct.length);
      out.set(iv);
      out.set(ct, 12);
      return PREFIX + toB64(out);
    },
    async decode(text, aad) {
      // Rows written before encryption was turned on are plain JSON.
      if (!text.startsWith(PREFIX)) return JSON.parse(text);
      const raw = fromB64(text.slice(PREFIX.length));
      const plain = await subtle().decrypt({ name: 'AES-GCM', iv: raw.subarray(0, 12), additionalData: enc.encode(aad) }, key, raw.subarray(12));
      return JSON.parse(dec.decode(plain));
    },
  };
}

/** A key from 32 bytes the app keeps safely (iOS Keychain, Android Keystore, OS keyring). */
export async function keyFromBytes(bytes: Uint8Array): Promise<CryptoKey> {
  if (bytes.length !== 32) throw new Error('the key must be 32 bytes');
  return subtle().importKey('raw', new Uint8Array(bytes), { name: 'AES-GCM' }, false, ['encrypt', 'decrypt']);
}

/** 32 random bytes for a new key (store them in the keychain, then `keyFromBytes`). */
export function newKeyBytes(): Uint8Array {
  return globalThis.crypto.getRandomValues(new Uint8Array(32));
}

/**
 * Browser: a non-extractable key for this account, created on first use
 * and kept in IndexedDB. Script on the page can use it but never read it
 * out; deleting it (`forgetBrowserKey`) makes what was stored unreadable.
 */
export async function browserKey(account: string): Promise<CryptoKey> {
  const db = await openKeys();
  const existing = await idbReq<CryptoKey | undefined>(db.transaction('keys', 'readonly').objectStore('keys').get(account));
  if (existing) return existing;
  const key = await subtle().generateKey({ name: 'AES-GCM', length: 256 }, false, ['encrypt', 'decrypt']);
  const t = db.transaction('keys', 'readwrite');
  // Another tab may have made one first: keep theirs.
  const store = t.objectStore('keys');
  const raced = await idbReq<CryptoKey | undefined>(store.get(account));
  if (raced) return raced;
  await idbReq(store.put(key, account));
  return key;
}

export async function forgetBrowserKey(account: string): Promise<void> {
  const db = await openKeys();
  await idbReq(db.transaction('keys', 'readwrite').objectStore('keys').delete(account));
}

function openKeys(): Promise<IDBDatabase> {
  return new Promise((resolve, reject) => {
    const r = indexedDB.open('agentim-keys', 1);
    r.onupgradeneeded = () => r.result.createObjectStore('keys');
    r.onsuccess = () => resolve(r.result);
    r.onerror = () => reject(r.error);
  });
}

function idbReq<T>(r: IDBRequest): Promise<T> {
  return new Promise((resolve, reject) => {
    r.onsuccess = () => resolve(r.result as T);
    r.onerror = () => reject(r.error);
  });
}

/** The app's own encryption of the stored text (any library). */
export function customCodec(f: { encrypt(plain: string): string | Promise<string>; decrypt(cipher: string): string | Promise<string> }): Codec {
  return {
    async encode(value, aad) {
      return PREFIX + (await f.encrypt(JSON.stringify({ a: aad, v: value })));
    },
    async decode(text, aad) {
      if (!text.startsWith(PREFIX)) return JSON.parse(text);
      const o = JSON.parse(await f.decrypt(text.slice(PREFIX.length)));
      if (o.a !== aad) throw new Error('stored content does not belong here');
      return o.v;
    },
  };
}
