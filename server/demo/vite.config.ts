import { defineConfig } from 'vite';
import react from '@vitejs/plugin-react';

// The demo (docs/12 §3.4), served by `agentim --demo` at /. `npm run dev`
// proxies the demo login and the WebSocket to a local server.
export default defineConfig({
  base: '/',
  plugins: [react()],
  // The SDK packages are linked from sdk/: one React, one client.
  resolve: { dedupe: ['react', 'react-dom', '@agentimhq/client-web'] },
  build: { outDir: 'dist', emptyOutDir: true, sourcemap: false, target: 'es2022' },
  server: { proxy: { '/api': 'http://127.0.0.1:8080', '/ws': { target: 'ws://127.0.0.1:8080', ws: true } } },
});
