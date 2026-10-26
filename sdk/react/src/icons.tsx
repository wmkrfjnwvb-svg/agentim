// The few icons the components draw: 24px line icons, inline SVG (no icon
// font or library). Decorative: the control around them carries the label.

import type { ReactNode } from 'react';

function Svg({ children, size = 20 }: { children: ReactNode; size?: number }) {
  return (
    <svg className="aim-icon" width={size} height={size} viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round" aria-hidden="true" focusable="false">
      {children}
    </svg>
  );
}

export const ArrowUp = () => (
  <Svg>
    <path d="M12 19V5" />
    <path d="m5 12 7-7 7 7" />
  </Svg>
);
export const ArrowDown = () => (
  <Svg size={16}>
    <path d="M12 5v14" />
    <path d="m19 12-7 7-7-7" />
  </Svg>
);
export const Check = () => (
  <Svg size={14}>
    <path d="M20 6 9 17l-5-5" />
  </Svg>
);
export const Alert = () => (
  <Svg size={14}>
    <circle cx="12" cy="12" r="10" />
    <path d="M12 8v4" />
    <path d="M12 16h.01" />
  </Svg>
);
export const Search = () => (
  <Svg size={16}>
    <circle cx="11" cy="11" r="7" />
    <path d="m20 20-3.5-3.5" />
  </Svg>
);
export const Sparkle = () => (
  <Svg size={12}>
    <path d="M12 3l1.9 5.1L19 10l-5.1 1.9L12 17l-1.9-5.1L5 10l5.1-1.9z" />
  </Svg>
);
export const ThumbUp = () => (
  <Svg size={16}>
    <path d="M7 10v11" />
    <path d="M15 5.9 14 10h5.8a2 2 0 0 1 2 2.3l-1.4 7A2 2 0 0 1 18.4 21H4a2 2 0 0 1-2-2v-7a2 2 0 0 1 2-2h2.8a2 2 0 0 0 1.8-1.1L12 2a3 3 0 0 1 3 3.9Z" />
  </Svg>
);
export const ThumbDown = () => (
  <Svg size={16}>
    <path d="M17 14V3" />
    <path d="M9 18.1 10 14H4.2a2 2 0 0 1-2-2.3l1.4-7A2 2 0 0 1 5.6 3H20a2 2 0 0 1 2 2v7a2 2 0 0 1-2 2h-2.8a2 2 0 0 0-1.8 1.1L12 22a3 3 0 0 1-3-3.9Z" />
  </Svg>
);
export const Link = () => (
  <Svg size={12}>
    <path d="M15 3h6v6" />
    <path d="M10 14 21 3" />
    <path d="M18 13v6a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2V8a2 2 0 0 1 2-2h6" />
  </Svg>
);
export const Stop = () => (
  <Svg size={12}>
    <rect x="6" y="6" width="12" height="12" rx="2" />
  </Svg>
);
