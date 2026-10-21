// Small building blocks shared by the pages.

import { useCallback, useEffect, useState, type ReactNode } from 'react';
import { ApiError } from './api';

export function useLoad<T>(load: () => Promise<T>, deps: unknown[]): [T | null, string | null, () => void] {
  const [v, setV] = useState<T | null>(null);
  const [err, setErr] = useState<string | null>(null);
  const [n, setN] = useState(0);
  useEffect(() => {
    let live = true;
    setErr(null);
    load().then(
      (x) => live && setV(x),
      (e) => live && setErr(message(e)),
    );
    return () => {
      live = false;
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [...deps, n]);
  return [v, err, useCallback(() => setN((x) => x + 1), [])];
}

export function message(e: unknown): string {
  if (e instanceof ApiError) return `${e.code}: ${e.message}`;
  return e instanceof Error ? e.message : String(e);
}

/// Runs an action and shows its error or result next to the button.
export function useAction() {
  const [busy, setBusy] = useState(false);
  const [err, setErr] = useState<string | null>(null);
  const run = async (f: () => Promise<unknown>) => {
    setBusy(true);
    setErr(null);
    try {
      await f();
    } catch (e) {
      setErr(message(e));
    } finally {
      setBusy(false);
    }
  };
  return { busy, err, run, setErr };
}

export function Err({ text }: { text: string | null }) {
  return text ? (
    <div className="error" role="alert">
      {text}
    </div>
  ) : null;
}

export function Page({ title, children, actions }: { title: string; children: ReactNode; actions?: ReactNode }) {
  return (
    <section className="page">
      <header className="page-head">
        <h1>{title}</h1>
        <div className="actions">{actions}</div>
      </header>
      {children}
    </section>
  );
}

/// A secret shown once, with a copy button.
export function Secret({ label, value }: { label: string; value: string }) {
  const [copied, setCopied] = useState(false);
  return (
    <div className="secret" data-testid="secret">
      <div>
        <strong>{label}</strong> — shown once; store it now.
      </div>
      <code>{value}</code>
      <button
        type="button"
        onClick={() => {
          navigator.clipboard?.writeText(value).then(() => setCopied(true), () => {});
        }}
      >
        {copied ? 'Copied' : 'Copy'}
      </button>
    </div>
  );
}

export function time(ms: number | null | undefined): string {
  if (!ms) return '—';
  return new Date(ms).toLocaleString();
}

export function Json({ value }: { value: unknown }) {
  return <pre className="json">{JSON.stringify(value, null, 2)}</pre>;
}
