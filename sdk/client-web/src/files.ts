// Files in the browser (docs/02-architecture.md §2.7): before the core
// sees a file, an image loses its EXIF and XMP (GPS) through the core's
// own `media.rs` (wasm) and gets a thumbnail (createImageBitmap +
// OffscreenCanvas); the core's `put`s are then carried out as PUTs of a
// slice of the Blob, with progress where XMLHttpRequest is there.
//
// Sources are kept by key: in memory, and in IndexedDB up to 200 MiB so an
// upload carries on after a reload (larger ones must be picked again).

import { stripMetadata } from './wasm.js';

/** Largest file kept in IndexedDB for after a reload. */
export const KEEP_MAX = 200 * 1024 * 1024;
/** Images up to this size have their metadata removed (it reads them whole). */
export const STRIP_MAX = 256 * 1024 * 1024;
export const THUMB_SIDE = 320;
export const THUMB_MAX_PIXELS = 40_000_000;

function sniff(b: Uint8Array): string | null {
  if (b[0] === 0xff && b[1] === 0xd8 && b[2] === 0xff) return 'image/jpeg';
  if (b[0] === 0x89 && b[1] === 0x50 && b[2] === 0x4e && b[3] === 0x47) return 'image/png';
  if (String.fromCharCode(...b.slice(0, 4)) === 'RIFF' && String.fromCharCode(...b.slice(8, 12)) === 'WEBP') return 'image/webp';
  if (String.fromCharCode(...b.slice(0, 4)) === 'GIF8') return 'image/gif';
  return null;
}

const newKey = () => `src_${Date.now().toString(36)}_${crypto.getRandomValues(new Uint32Array(2)).reduce((s, x) => s + x.toString(36), '')}`;

/** Where the sources of uploads are kept. */
export class FileSources {
  private mem = new Map<string, Blob>();
  private db: Promise<IDBDatabase | null>;

  constructor(name: string | null, idb: IDBFactory | undefined = globalThis.indexedDB) {
    this.db =
      name && idb
        ? new Promise((ok) => {
            const r = idb.open(`${name}-files`, 1);
            r.onupgradeneeded = () => r.result.createObjectStore('blobs');
            r.onsuccess = () => ok(r.result);
            r.onerror = () => ok(null);
          })
        : Promise.resolve(null);
  }

  private async tx<T>(mode: IDBTransactionMode, f: (s: IDBObjectStore) => IDBRequest<T>): Promise<T | undefined> {
    const db = await this.db;
    if (!db) return undefined;
    return new Promise((ok) => {
      try {
        const r = f(db.transaction('blobs', mode).objectStore('blobs'));
        r.onsuccess = () => ok(r.result);
        r.onerror = () => ok(undefined);
      } catch {
        ok(undefined);
      }
    });
  }

  async put(key: string, b: Blob) {
    this.mem.set(key, b);
    if (b.size <= KEEP_MAX) await this.tx('readwrite', (s) => s.put(b, key));
  }

  async get(key: string): Promise<Blob | undefined> {
    return this.mem.get(key) ?? ((await this.tx('readonly', (s) => s.get(key))) as Blob | undefined);
  }

  async remove(key: string) {
    this.mem.delete(key);
    await this.tx('readwrite', (s) => s.delete(key));
  }

  /** Drops kept sources no queued upload names (left by an earlier run). */
  async sweep(keep: Set<string>) {
    const keys = ((await this.tx('readonly', (s) => s.getAllKeys())) ?? []) as string[];
    for (const k of keys) if (!keep.has(String(k))) await this.remove(String(k));
  }
}

/** A thumbnail: upright, longest side 320; JPEG, or PNG with transparency. */
async function thumbnail(b: Blob, mime: string): Promise<{ thumb?: { blob: Blob; width: number; height: number }; width?: number; height?: number }> {
  const g = globalThis as any;
  if (!g.createImageBitmap) return {};
  let bmp: ImageBitmap;
  try {
    bmp = await g.createImageBitmap(b, { imageOrientation: 'from-image' });
  } catch {
    return {};
  }
  const { width, height } = bmp;
  if (width * height > THUMB_MAX_PIXELS || Math.max(width, height) <= THUMB_SIDE || !g.OffscreenCanvas) {
    bmp.close?.();
    return { width, height };
  }
  const k = THUMB_SIDE / Math.max(width, height);
  const tw = Math.max(1, Math.round(width * k));
  const th = Math.max(1, Math.round(height * k));
  const canvas = new g.OffscreenCanvas(tw, th);
  const ctx = canvas.getContext('2d');
  ctx.drawImage(bmp, 0, 0, tw, th);
  bmp.close?.();
  let alpha = false;
  if (mime !== 'image/jpeg') {
    const px = ctx.getImageData(0, 0, tw, th).data as Uint8ClampedArray;
    for (let i = 3; i < px.length && !alpha; i += 4) alpha = px[i] < 255;
  }
  const blob: Blob = await canvas.convertToBlob(alpha ? { type: 'image/png' } : { type: 'image/jpeg', quality: 0.75 });
  return { thumb: { blob, width: tw, height: th }, width, height };
}

export interface Prepared {
  cmd: Record<string, unknown>;
  blobs: Record<string, Blob>;
}

/** The app's file as the core's `send_file`, with the blobs to keep. */
export async function prepare(
  convId: string,
  file: Blob,
  o: { name?: string; caption?: string; replyTo?: string; clientMsgId: string; keepMetadata?: boolean; wasm?: unknown },
): Promise<Prepared> {
  const name = o.name ?? (file as File).name ?? 'file';
  const head = new Uint8Array(await file.slice(0, 16).arrayBuffer());
  const image = sniff(head);
  let source: Blob = file;
  const f: Record<string, unknown> = { name, mime: image ?? (file.type || undefined) };
  const blobs: Record<string, Blob> = {};
  const cmd: Record<string, unknown> = { cmd: 'send_file', conv_id: convId, caption: o.caption, reply_to: o.replyTo, client_msg_id: o.clientMsgId };
  if (image && file.size <= STRIP_MAX) {
    if (!o.keepMetadata) {
      const clean = await stripMetadata(new Uint8Array(await file.arrayBuffer()), o.wasm as any);
      if (clean) source = new Blob([clean as BlobPart], { type: image });
    }
    const t = await thumbnail(source, image);
    if (t.width) Object.assign(f, { width: t.width, height: t.height });
    if (t.thumb) {
      const k = newKey();
      blobs[k] = t.thumb.blob;
      cmd.thumb = { source: k, size: t.thumb.blob.size, width: t.thumb.width, height: t.thumb.height };
    }
  }
  const key = newKey();
  blobs[key] = source;
  cmd.file = { ...f, source: key, size: source.size };
  return { cmd, blobs };
}

export type PutResult = { etag: string } | { error: { status?: number; unreadable?: boolean; detail: string } };

/** One PUT of a slice; `progress` a few times a second where it can. */
export function put(
  url: string,
  headers: Record<string, string>,
  body: Blob,
  progress: (sent: number) => void,
): { done: Promise<PutResult>; abort: () => void } {
  const g = globalThis as any;
  // Browsers set Content-Length themselves (and refuse it as a header).
  const h = Object.entries(headers).filter(([k]) => k.toLowerCase() !== 'content-length');
  if (g.XMLHttpRequest) {
    const x: XMLHttpRequest = new g.XMLHttpRequest();
    const done = new Promise<PutResult>((ok) => {
      let last = 0;
      x.upload.onprogress = (e) => {
        if (Date.now() - last >= 250) {
          last = Date.now();
          progress(e.loaded);
        }
      };
      x.onload = () =>
        ok(x.status >= 200 && x.status < 300 ? { etag: x.getResponseHeader('ETag') ?? '' } : { error: { status: x.status, detail: String(x.responseText ?? '').slice(0, 300) } });
      x.onerror = () => ok({ error: { detail: 'network error' } });
      x.onabort = () => ok({ error: { detail: 'aborted' } });
    });
    x.open('PUT', url);
    for (const [k, v] of h) x.setRequestHeader(k, v);
    x.send(body);
    return { done, abort: () => x.abort() };
  }
  const ac = new AbortController();
  const done = fetch(url, { method: 'PUT', headers: Object.fromEntries(h), body, signal: ac.signal }).then(
    async (r): Promise<PutResult> => (r.ok ? { etag: r.headers.get('etag') ?? '' } : { error: { status: r.status, detail: (await r.text().catch(() => '')).slice(0, 300) } }),
    (e): PutResult => ({ error: { detail: String(e) } }),
  );
  return { done, abort: () => ac.abort() };
}
