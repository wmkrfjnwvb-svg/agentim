// agentim.co (docs/en/design/developer-experience.md): the homepage (src/pages/index.astro) and the
// documentation under /docs (Starlight). No analytics, no third-party
// scripts, no cookies; fonts are the system's.
import { defineConfig } from 'astro/config';
import react from '@astrojs/react';
import starlight from '@astrojs/starlight';
import links from 'starlight-links-validator';

// Sidebar entries: a group by directory, a page by slug, and a labelled group of pages.
const zh = (en, z) => ({ label: en, translations: { 'zh-CN': z } });
const dir = (en, z, d, collapsed = true) => ({ ...zh(en, z), collapsed, items: [{ autogenerate: { directory: `docs/${d}` } }] });
const pages = (en, z, slugs, collapsed = false) => ({ ...zh(en, z), collapsed, items: slugs.map((slug) => (typeof slug === 'string' ? { slug: `docs/${slug}` } : slug)) });

export default defineConfig({
  site: 'https://agentim.co',
  trailingSlash: 'ignore',
  // The homepage renders sdk/react from source: its react is this site's.
  vite: { resolve: { dedupe: ['react', 'react-dom'] } },
  integrations: [
    react(),
    starlight({
      title: 'AgentIM',
      description: 'Open-source chat for people and AI agents: a self-hosted IM server with SDKs and UI components for every platform.',
      logo: { light: './src/assets/logo-light.svg', dark: './src/assets/logo-dark.svg', replacesTitle: true },
      favicon: '/favicon.svg',
      social: [{ icon: 'github', label: 'GitHub', href: 'https://github.com/wmkrfjnwvb-svg/agentim' }],
      editLink: { baseUrl: 'https://github.com/wmkrfjnwvb-svg/agentim/edit/main/site/' },
      customCss: ['@fontsource-variable/inter', './src/styles/theme.css'],
      lastUpdated: true,
      // Untranslated Chinese pages fall back to English, so links to them are
      // valid; the marketing pages are Astro pages outside the docs.
      plugins: [links({ errorOnFallbackPages: false, exclude: ['/', '/zh/', '/cloud/', '/zh/cloud/', '/pricing/', '/zh/pricing/', '/benchmarks/', '/zh/benchmarks/'] })],
      defaultLocale: 'root',
      locales: {
        root: { label: 'English', lang: 'en' },
        zh: { label: '简体中文', lang: 'zh-CN' },
      },
      // Organized by capability (docs/24 §6 in the private repo): what the
      // product does first, then SDKs, running it, and the references.
      sidebar: [
        pages('Get started', '开始', [{ slug: 'docs', ...zh('Documentation home', '文档首页') }, 'concepts/overview', 'get-started/quickstart', 'get-started/platforms']),
        pages('Messaging', 'IM 功能', ['concepts/conversations', 'concepts/permissions', 'concepts/threads-reactions-pins', 'concepts/sync-and-offline', 'concepts/files', 'concepts/devices-and-login', 'guides/before-send']),
        pages('AI agents', 'AI Agent', ['concepts/agents', 'concepts/streaming', 'ai/roles-and-skills', 'ai/connections', 'guides/built-in-agents', 'concepts/approvals', 'concepts/guardrails', 'guides/chat-components', 'guides/migrate-agent']),
        pages('UI components', 'UI 组件', ['client-sdks/react-api', 'client-sdks/react-native-ui-api', 'guides/theming'], true),
        { ...zh('SDKs', 'SDK'), collapsed: true, items: [dir('Client SDKs', '客户端 SDK', 'client-sdks'), dir('Agent SDKs', 'Agent SDK', 'agent-sdks'), dir('Server SDKs', '服务端 SDK', 'server-sdks')] },
        { ...zh('Deploy and operate', '部署和运维'), collapsed: true, items: [{ autogenerate: { directory: 'docs/operations' } }, { slug: 'docs/guides/launch-checklist' }] },
        dir('Security and compliance', '安全和合规', 'security'),
        { ...zh('Reference', '参考'), collapsed: true, items: [{ autogenerate: { directory: 'docs/reference' } }, { slug: 'docs/concepts/protocol-versions' }] },
        { ...zh('Legal', '法律'), collapsed: true, items: [{ autogenerate: { directory: 'legal' } }] },
      ],
    }),
  ],
});
