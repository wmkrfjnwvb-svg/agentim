// Images and files in the chat (docs/02-architecture.md §2.7): the image
// as its thumbnail at its own proportions (at most 240 × 240), opening a
// viewer with the enlarged thumbnail, "View original" and "Download"; a
// file as a card with its type's icon, name and size. Both show the
// upload while it runs.

import { useEffect, useRef, useState, type ReactNode } from 'react';
import { createPortal } from 'react-dom';
import type { Message } from '@agentimhq/client-web';
import { useClient, useFileUrl } from './hooks.js';
import type { Strings } from './strings.js';
import { fileType, fitBox, formatBytes, shortName } from './format.js';
import { Close, Download, FileIcon, Paperclip } from './icons.js';

function percent(m: Message): number | undefined {
  const u = (m as any).upload;
  if (!u || m.status !== 'uploading' || !u.size) return undefined;
  return Math.min(100, Math.floor((u.sent / u.size) * 100));
}

/** A circle filling up while the file goes up. */
function Progress({ pct, label }: { pct: number; label: string }) {
  const r = 16;
  const c = 2 * Math.PI * r;
  return (
    <span className="aim-progress" role="progressbar" aria-valuemin={0} aria-valuemax={100} aria-valuenow={pct} aria-label={label}>
      <svg width="40" height="40" viewBox="0 0 40 40" aria-hidden="true">
        <circle cx="20" cy="20" r={r} fill="none" strokeWidth="3" className="aim-progress-track" />
        <circle cx="20" cy="20" r={r} fill="none" strokeWidth="3" className="aim-progress-bar" strokeDasharray={c} strokeDashoffset={c * (1 - pct / 100)} transform="rotate(-90 20 20)" />
      </svg>
    </span>
  );
}

/** Reads `url` into a Blob, saying how far it is. */
async function fetchProgress(url: string, onProgress: (pct: number) => void): Promise<Blob> {
  const r = await fetch(url);
  if (!r.ok) throw new Error(`HTTP ${r.status}`);
  const total = Number(r.headers.get('content-length')) || 0;
  if (!r.body || !total) return r.blob();
  const reader = r.body.getReader();
  const chunks: Uint8Array[] = [];
  let got = 0;
  for (;;) {
    const { done, value } = await reader.read();
    if (done) break;
    chunks.push(value);
    got += value.length;
    onProgress(Math.min(99, Math.floor((got / total) * 100)));
  }
  return new Blob(chunks as BlobPart[], { type: r.headers.get('content-type') ?? '' });
}

function save(href: string, name: string) {
  const a = document.createElement('a');
  a.href = href;
  a.download = name;
  a.rel = 'noopener';
  document.body.appendChild(a);
  a.click();
  a.remove();
}

/** Downloads a file under its name (a cross-origin link ignores `download`, so through a Blob). */
async function download(url: string, name: string, onProgress?: (pct: number) => void) {
  try {
    const b = await fetchProgress(url, onProgress ?? (() => {}));
    const local = URL.createObjectURL(b);
    save(local, name);
    setTimeout(() => URL.revokeObjectURL(local), 30_000);
  } catch {
    // The store without CORS: let the browser fetch it.
    save(url, name);
  }
}

/** The enlarged thumbnail; "View original" loads the full image in its place. */
export function ImageViewer({ convId, message: m, thumb, strings, onClose }: { convId: string; message: Message; thumb?: string; strings: Strings; onClose(): void }) {
  const client = useClient();
  const p = m.payload ?? {};
  const [original, setOriginal] = useState<string>();
  const [loading, setLoading] = useState<number>();
  const [failed, setFailed] = useState(false);
  const close = useRef<HTMLButtonElement>(null);
  useEffect(() => {
    const prev = document.activeElement as HTMLElement | null;
    close.current?.focus();
    const key = (e: globalThis.KeyboardEvent) => {
      if (e.key === 'Escape') onClose();
    };
    document.addEventListener('keydown', key);
    return () => {
      document.removeEventListener('keydown', key);
      prev?.focus?.();
    };
  }, [onClose]);
  useEffect(() => () => void (original && URL.revokeObjectURL(original)), [original]);
  const url = async () => {
    const local = client.localPreview?.(p.local_source);
    if (local) return local;
    return (await client.fileUrl!(convId, p.file_id, 'original')).url;
  };
  const viewOriginal = async () => {
    setLoading(0);
    setFailed(false);
    try {
      const b = await fetchProgress(await url(), setLoading);
      setOriginal(URL.createObjectURL(b));
    } catch {
      setFailed(true);
    } finally {
      setLoading(undefined);
    }
  };
  const name = p.name ?? 'image';
  // Swipe down on a phone closes it.
  const start = useRef<number | null>(null);
  return (
    <div
      className="aim-viewer"
      role="dialog"
      aria-modal="true"
      aria-label={p.caption || name}
      onClick={(e) => e.target === e.currentTarget && onClose()}
      onTouchStart={(e) => (start.current = e.touches[0]?.clientY ?? null)}
      onTouchEnd={(e) => {
        const y = e.changedTouches[0]?.clientY;
        if (start.current !== null && y !== undefined && y - start.current > 80) onClose();
        start.current = null;
      }}
    >
      <div className="aim-viewer-bar">
        <span className="aim-viewer-name" dir="auto">
          {shortName(name, 48)}
        </span>
        {!original ? (
          <button type="button" className="aim-viewer-original" onClick={() => void viewOriginal()} disabled={loading !== undefined}>
            {loading !== undefined ? `${strings.loading} ${loading}%` : strings.viewOriginal}
          </button>
        ) : null}
        <button type="button" aria-label={strings.download} title={strings.download} onClick={() => void url().then((u) => download(u, name))}>
          <Download />
        </button>
        <button ref={close} type="button" aria-label={strings.close} title={strings.close} onClick={onClose}>
          <Close />
        </button>
      </div>
      <img className="aim-viewer-img" src={original ?? thumb} alt={p.caption || name} data-original={original ? true : undefined} />
      {failed ? (
        <div className="aim-viewer-caption" role="alert">
          {strings.fileUnavailable}
        </div>
      ) : p.caption ? (
        <div className="aim-viewer-caption" dir="auto">
          {p.caption}
        </div>
      ) : null}
    </div>
  );
}

/**
 * An image message: the thumbnail at its proportions (at most 240 × 240) with its caption and
 * upload progress; tapping opens the viewer.
 */
export function ImageMessage({ convId, message: m, strings }: { convId: string; message: Message; strings: Strings }) {
  const p = m.payload ?? {};
  const { url, error } = useFileUrl(convId, p, 'thumb');
  const [open, setOpen] = useState(false);
  const [loaded, setLoaded] = useState(false);
  const box = fitBox(p.width, p.height);
  const pct = percent(m);
  return (
    <>
      <button
        type="button"
        className="aim-image"
        style={{ width: box.width, height: box.height }}
        aria-label={p.caption ? `${strings.image}: ${p.caption}` : strings.image}
        data-loaded={loaded || undefined}
        disabled={!url || !!error}
        onClick={() => setOpen(true)}
      >
        {url && !error ? <img src={url} alt="" onLoad={() => setLoaded(true)} draggable={false} /> : null}
        {error ? <span className="aim-image-gone">{strings.fileUnavailable}</span> : null}
        {pct !== undefined ? <Progress pct={pct} label={strings.uploading(pct)} /> : null}
      </button>
      {p.caption ? (
        <div className="aim-text aim-caption" dir="auto">
          {p.caption}
        </div>
      ) : null}
      {open
        ? // Over everything: the list around it may clip or contain it.
          createPortal(<ImageViewer convId={convId} message={m} thumb={url} strings={strings} onClose={() => setOpen(false)} />, document.body)
        : null}
    </>
  );
}

/**
 * A file message: a card with the type's icon, the name, the size and upload progress; clicking
 * downloads the file.
 */
export function FileMessage({ convId, message: m, strings }: { convId: string; message: Message; strings: Strings }) {
  const client = useClient();
  const p = m.payload ?? {};
  const type = fileType(p.name, p.mime);
  const pct = percent(m);
  const [loading, setLoading] = useState<number>();
  const [gone, setGone] = useState(false);
  const ready = !!p.file_id && m.status !== 'uploading';
  const open = async () => {
    if (!ready || !client.fileUrl) return;
    setLoading(0);
    try {
      const u = await client.fileUrl(convId, p.file_id, 'original');
      await download(u.url, p.name ?? 'file', setLoading);
    } catch {
      setGone(true);
    } finally {
      setLoading(undefined);
    }
  };
  const detail: ReactNode = gone
    ? strings.fileUnavailable
    : pct !== undefined
      ? strings.uploading(pct)
      : loading !== undefined
        ? `${strings.loading} ${loading}%`
        : `${formatBytes(p.size ?? 0)} · ${strings.fileTypes[type] ?? strings.fileTypes.other}`;
  return (
    <>
      <button type="button" className="aim-file" data-type={type} onClick={() => void open()} disabled={!ready || gone} title={p.name}>
        <FileIcon type={type} />
        <span className="aim-file-body">
          <span className="aim-file-name" dir="auto">
            {shortName(p.name ?? 'file')}
          </span>
          <span className="aim-file-detail">{detail}</span>
          {pct !== undefined ? (
            <span className="aim-file-bar" role="progressbar" aria-valuemin={0} aria-valuemax={100} aria-valuenow={pct} aria-label={strings.uploading(pct)}>
              <span style={{ width: `${pct}%` }} />
            </span>
          ) : null}
        </span>
        {ready && !gone ? (
          <span className="aim-file-get" aria-hidden="true">
            <Download />
          </span>
        ) : null}
      </button>
      {p.caption ? (
        <div className="aim-text aim-caption" dir="auto">
          {p.caption}
        </div>
      ) : null}
    </>
  );
}

/** The paperclip: picks files and sends each. */
export function AttachButton({ strings, onFiles }: { strings: Strings; onFiles(files: File[]): void }) {
  const input = useRef<HTMLInputElement>(null);
  return (
    <>
      <button type="button" className="aim-attach" aria-label={strings.attach} title={strings.attach} onClick={() => input.current?.click()}>
        <Paperclip />
      </button>
      <input
        ref={input}
        type="file"
        multiple
        hidden
        onChange={(e) => {
          const files = Array.from(e.target.files ?? []);
          e.target.value = '';
          if (files.length) onFiles(files);
        }}
      />
    </>
  );
}
