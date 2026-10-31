// The admin API (/admin/v1, docs/12 §4). The session is an HttpOnly
// cookie; every change carries the CSRF token the login returned.

export type Role = 'owner' | 'developer' | 'operator' | 'moderator';
export interface Me {
  admin: { name: string; role: Role };
  csrf: string | null;
  totp?: boolean;
}

export class ApiError extends Error {
  constructor(
    public status: number,
    public code: string,
    message: string,
  ) {
    super(message);
  }
}

let csrf: string | null = null;
let onUnauthorized: () => void = () => {};

export function setCsrf(t: string | null) {
  csrf = t;
}
export function whenLoggedOut(f: () => void) {
  onUnauthorized = f;
}

export async function api<T = any>(method: string, path: string, body?: unknown): Promise<T> {
  const headers: Record<string, string> = {};
  if (body !== undefined) headers['content-type'] = 'application/json';
  if (method !== 'GET' && csrf) headers['x-csrf-token'] = csrf;
  const r = await fetch(`/admin/v1${path}`, {
    method,
    headers,
    credentials: 'same-origin',
    body: body === undefined ? undefined : JSON.stringify(body),
  });
  const text = await r.text();
  let v: any = null;
  try {
    v = text ? JSON.parse(text) : null;
  } catch {
    v = text;
  }
  if (!r.ok) {
    const code = v?.code ?? 'error';
    if (r.status === 401 && code === 'unauthorized' && !path.startsWith('/login') && !path.startsWith('/setup')) {
      onUnauthorized();
    }
    throw new ApiError(r.status, code, v?.detail ?? r.statusText);
  }
  return v as T;
}

/// Query string from the defined values.
export function qs(o: Record<string, string | number | undefined | null>): string {
  const p = Object.entries(o).filter(([, v]) => v !== undefined && v !== null && v !== '');
  return p.length ? '?' + p.map(([k, v]) => `${k}=${encodeURIComponent(String(v))}`).join('&') : '';
}

export const enc = encodeURIComponent;

export const can = {
  app: (r: Role) => r === 'owner' || r === 'developer',
  appRead: (r: Role) => r === 'owner' || r === 'developer' || r === 'moderator',
  deploy: (r: Role) => r === 'owner' || r === 'operator',
  owner: (r: Role) => r === 'owner',
  trace: (r: Role) => r === 'owner' || r === 'operator' || r === 'moderator',
};
