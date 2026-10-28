// The Rust core compiled to wasm (sdk/client-rust/wasm): JSON in, JSON
// out through linear memory. See agentim_client_core::wire for the shapes.

export type Effect = Record<string, any>;

interface Exports {
  memory: WebAssembly.Memory;
  agentim_alloc(len: number): number;
  agentim_dealloc(ptr: number, len: number): void;
  agentim_new(ptr: number, len: number): number;
  agentim_handle(h: number, ptr: number, len: number, now: number): bigint;
  agentim_next_deadline(h: number): number;
  agentim_free(h: number): void;
  agentim_strip_metadata(ptr: number, len: number): bigint;
}

const enc = new TextEncoder();
const dec = new TextDecoder();

let compiled: Promise<WebAssembly.Module> | null = null;

/** Where the module comes from: bytes, a URL, or (default) next to this file. */
export type WasmSource = BufferSource | URL | string;

async function bytesOf(src?: WasmSource): Promise<BufferSource> {
  if (src instanceof ArrayBuffer || ArrayBuffer.isView(src)) return src;
  const url = src ? new URL(String(src), import.meta.url) : new URL('./agentim_core.wasm', import.meta.url);
  if (url.protocol === 'file:') {
    // Node: read the file (kept out of browser bundles).
    const name = 'node:fs/promises';
    const fs: any = await import(name);
    return fs.readFile(url);
  }
  const r = await fetch(url);
  if (!r.ok) throw new Error(`could not load the AgentIM core: ${r.status}`);
  return r.arrayBuffer();
}

export function compile(src?: WasmSource): Promise<WebAssembly.Module> {
  if (!compiled || src) compiled = bytesOf(src).then((b) => WebAssembly.compile(b));
  return compiled;
}

/** One core instance. */
export class WasmCore {
  private constructor(
    private x: Exports,
    private h: number,
  ) {}

  static async create(config: object, src?: WasmSource): Promise<WasmCore> {
    const inst = await WebAssembly.instantiate(await compile(src), {});
    const x = inst.exports as unknown as Exports;
    const core = new WasmCore(x, 0);
    const h = core.call((p, l) => x.agentim_new(p, l), JSON.stringify(config));
    if (!h) throw new Error('invalid client configuration');
    core.h = h;
    return core;
  }

  private call<T>(f: (ptr: number, len: number) => T, text: string): T {
    const b = enc.encode(text);
    const p = this.x.agentim_alloc(b.length);
    new Uint8Array(this.x.memory.buffer, p, b.length).set(b);
    try {
      return f(p, b.length);
    } finally {
      this.x.agentim_dealloc(p, b.length);
    }
  }

  handle(input: unknown, now = Date.now()): Effect[] {
    const r = this.call((p, l) => this.x.agentim_handle(this.h, p, l, now), JSON.stringify(input));
    const ptr = Number(r >> 32n);
    const len = Number(r & 0xffffffffn);
    const text = dec.decode(new Uint8Array(this.x.memory.buffer, ptr, len));
    this.x.agentim_dealloc(ptr, len);
    return JSON.parse(text);
  }

  nextDeadline(): number | null {
    const d = this.x.agentim_next_deadline(this.h);
    return d < 0 ? null : d;
  }

  free() {
    this.x.agentim_free(this.h);
  }
}

let helper: Promise<Exports> | null = null;

/** The image without EXIF and XMP (the core's `media.rs`), or null when
 * there was nothing to remove. */
export async function stripMetadata(b: Uint8Array, src?: WasmSource): Promise<Uint8Array | null> {
  helper ??= compile(src).then(async (m) => (await WebAssembly.instantiate(m, {})).exports as unknown as Exports);
  const x = await helper;
  const p = x.agentim_alloc(b.length);
  new Uint8Array(x.memory.buffer, p, b.length).set(b);
  let r: bigint;
  try {
    r = x.agentim_strip_metadata(p, b.length);
  } finally {
    x.agentim_dealloc(p, b.length);
  }
  if (r === 0n) return null;
  const ptr = Number(r >> 32n);
  const len = Number(r & 0xffffffffn);
  const out = new Uint8Array(x.memory.buffer, ptr, len).slice();
  x.agentim_dealloc(ptr, len);
  return out;
}
