// agentim.dev (docs/12 §5): the homepage (src/pages/index.astro) and the
// documentation under /docs (Starlight). No analytics, no third-party
// scripts, no cookies; fonts are the system's.
import { defineConfig } from 'astro/config';
import react from '@astrojs/react';
import starlight from '@astrojs/starlight';
import links from 'starlight-links-validator';

const group = (label, dir, collapsed = false) => ({ label, collapsed, items: [{ autogenerate: { directory: `docs/${dir}` } }] });

export default defineConfig({
  site: 'https://agentim.dev',
  trailingSlash: 'ignore',
  integrations: [
    react(),
    starlight({
      title: 'AgentIM',
      description: 'Open-source chat for people and AI agents: a self-hosted IM server with SDKs and UI components for every platform.',
      logo: { light: './src/assets/logo-light.svg', dark: './src/assets/logo-dark.svg', replacesTitle: true },
      favicon: '/favicon.svg',
      social: [{ icon: 'github', label: 'GitHub', href: 'https://github.com/wmkrfjnwvb-svg/agentim' }],
      editLink: { baseUrl: 'https://github.com/wmkrfjnwvb-svg/agentim/edit/main/site/' },
      customCss: ['./src/styles/theme.css'],
      lastUpdated: true,
      plugins: [links()],
      sidebar: [
        group('Get started', 'get-started'),
        group('Concepts', 'concepts'),
        group('Guides', 'guides', true),
        group('Client SDKs', 'client-sdks', true),
        group('Agent SDKs', 'agent-sdks', true),
        group('Server SDKs', 'server-sdks', true),
        group('API reference', 'reference', true),
        group('Operations', 'operations', true),
        group('Security & compliance', 'security', true),
      ],
    }),
  ],
});
