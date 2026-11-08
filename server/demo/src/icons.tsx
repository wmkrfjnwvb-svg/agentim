// The demo's own icons (24px line icons, inline SVG) and the logo mark.

import type { ReactNode } from 'react';

function Svg({ children, size = 18 }: { children: ReactNode; size?: number }) {
  return (
    <svg width={size} height={size} viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round" aria-hidden="true" focusable="false">
      {children}
    </svg>
  );
}

export const Logo = () => (
  <svg className="logo" width="28" height="28" viewBox="0 0 28 28" aria-hidden="true" focusable="false">
    <defs>
      <linearGradient id="aim-logo" x1="0" y1="0" x2="1" y2="1">
        <stop offset="0" stopColor="#2563eb" />
        <stop offset="1" stopColor="#7c3aed" />
      </linearGradient>
    </defs>
    <rect width="28" height="28" rx="8" fill="url(#aim-logo)" />
    <path d="M8 9.5h12a1.5 1.5 0 0 1 1.5 1.5v6a1.5 1.5 0 0 1-1.5 1.5h-6.5L10 21v-2.5H8A1.5 1.5 0 0 1 6.5 17v-6A1.5 1.5 0 0 1 8 9.5Z" fill="#fff" />
    <circle cx="11" cy="14" r="1.2" fill="#2563eb" />
    <circle cx="14" cy="14" r="1.2" fill="#4f46e5" />
    <circle cx="17" cy="14" r="1.2" fill="#7c3aed" />
  </svg>
);

export const Info = () => (
  <Svg>
    <circle cx="12" cy="12" r="10" />
    <path d="M12 16v-4" />
    <path d="M12 8h.01" />
  </Svg>
);
export const Back = () => (
  <Svg>
    <path d="m15 18-6-6 6-6" />
  </Svg>
);
export const Close = () => (
  <Svg>
    <path d="M18 6 6 18" />
    <path d="m6 6 12 12" />
  </Svg>
);
export const LogOut = () => (
  <Svg>
    <path d="M9 21H5a2 2 0 0 1-2-2V5a2 2 0 0 1 2-2h4" />
    <path d="m16 17 5-5-5-5" />
    <path d="M21 12H9" />
  </Svg>
);
export const Code = () => (
  <Svg>
    <path d="m16 18 6-6-6-6" />
    <path d="m8 6-6 6 6 6" />
  </Svg>
);
