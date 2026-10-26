// The theme (docs/12 §3.1): the same tokens as the web components,
// light or dark following the system unless the app picks one, any
// colour overridable.

import { createContext, useContext, useMemo, type ReactNode } from 'react';
import { useColorScheme } from 'react-native';
import { avatar, dark, font, light, radius, space, target, type Colors } from './tokens';

export interface Theme {
  scheme: 'light' | 'dark';
  colors: Colors;
  avatar: string[];
  radius: typeof radius;
  space: typeof space;
  font: typeof font;
  /** The smallest touch target (Apple HIG: 44 pt). */
  target: number;
}

const Ctx = createContext<Theme | null>(null);

function make(scheme: 'light' | 'dark', colors?: Partial<Colors>): Theme {
  return { scheme, colors: { ...(scheme === 'dark' ? dark : light), ...colors }, avatar, radius, space, font, target };
}

export function AgentIMThemeProvider({ scheme = 'system', colors, children }: { scheme?: 'light' | 'dark' | 'system'; colors?: Partial<Colors>; children?: ReactNode }) {
  const system = useColorScheme();
  const s = scheme === 'system' ? (system === 'dark' ? 'dark' : 'light') : scheme;
  const theme = useMemo(() => make(s, colors), [s, colors]);
  return <Ctx.Provider value={theme}>{children}</Ctx.Provider>;
}

export function useTheme(): Theme {
  return useContext(Ctx) ?? make('light');
}
