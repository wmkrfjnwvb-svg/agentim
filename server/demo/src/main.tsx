import { StrictMode } from 'react';
import { createRoot } from 'react-dom/client';
import '@agentimhq/react/theme.css';
import './style.css';
import { Root } from './app';

createRoot(document.getElementById('root')!).render(
  <StrictMode>
    <Root />
  </StrictMode>,
);
