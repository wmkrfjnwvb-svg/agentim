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
export const Paperclip = () => (
  <Svg>
    <path d="m21.4 11.1-9.2 9.2a6 6 0 0 1-8.5-8.5l9.2-9.2a4 4 0 0 1 5.7 5.7l-9.2 9.2a2 2 0 0 1-2.8-2.8l8.5-8.5" />
  </Svg>
);
export const Download = () => (
  <Svg size={18}>
    <path d="M12 3v12" />
    <path d="m7 10 5 5 5-5" />
    <path d="M5 21h14" />
  </Svg>
);
export const Close = () => (
  <Svg>
    <path d="M18 6 6 18" />
    <path d="m6 6 12 12" />
  </Svg>
);

/** A page with a folded corner in the type's colour, its glyph in white (our own drawing). */
export function FileIcon({ type, size = 40 }: { type: string; size?: number }) {
  const glyph: Record<string, ReactNode> = {
    pdf: <path d="M9 13.5h1.5a1.25 1.25 0 0 0 0-2.5H9v5M13.5 11v5h1a2 2 0 0 0 2-2v-1a2 2 0 0 0-2-2z" />,
    doc: <path d="M8.5 11.5h7M8.5 14h7M8.5 16.5h4.5" />,
    sheet: <path d="M8 11h8v6H8zM8 14h8M12 11v6" />,
    slides: <path d="M8 11h8v5H8zM12 16v2M10 18h4" />,
    archive: <path d="M12 9.5v1M12 12v1M12 14.5v1M11 16.5h2v1.5h-2z" />,
    audio: <path d="M10.5 17.5a1.5 1.5 0 1 1-1.5-1.5h1.5V11l5-1v6a1.5 1.5 0 1 1-1.5-1.5H15" />,
    video: <path d="M8 11.5h6v5H8zM14 13l2.5-1.5v4L14 14" />,
    code: <path d="m10 11.5-2 2 2 2M14 11.5l2 2-2 2" />,
    image: <path d="M8 17l2.5-3 2 2 1.5-1.5L16 17zM10 11.2a.8.8 0 1 0 0 .01" />,
    other: <path d="M9 14h6" />,
  };
  return (
    <svg className="aim-file-icon" data-type={type} width={size} height={size} viewBox="0 0 24 24" aria-hidden="true" focusable="false">
      <path d="M6 2.5h8.5L19.5 7.5v12.5a1.5 1.5 0 0 1-1.5 1.5H6A1.5 1.5 0 0 1 4.5 20V4A1.5 1.5 0 0 1 6 2.5z" fill={`var(--aim-file-${type}, var(--aim-file-other))`} />
      <path d="M14.5 2.5v3.5a1.5 1.5 0 0 0 1.5 1.5h3.5" fill="rgba(255,255,255,0.35)" />
      <g fill="none" stroke="#fff" strokeWidth="1.4" strokeLinecap="round" strokeLinejoin="round">
        {glyph[type] ?? glyph.other}
      </g>
    </svg>
  );
}
