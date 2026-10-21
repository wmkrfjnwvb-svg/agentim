import { defineConfig } from 'vite';
import react from '@vitejs/plugin-react';

// Served by the agentim server at /console (docs/12 §4). `npm run dev`
// proxies the admin API to a local server.
export default defineConfig({
  base: '/console/',
  plugins: [react()],
  build: { outDir: 'dist', emptyOutDir: true, sourcemap: false },
  server: { proxy: { '/admin': 'http://127.0.0.1:8080' } },
});
