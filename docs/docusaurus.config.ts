import { themes as prismThemes } from 'prism-react-renderer';
import type { Config } from '@docusaurus/types';
import type * as Preset from '@docusaurus/preset-classic';

/**
 * Local developer documentation site for the Zarzour backend tree.
 *
 * Publication status: NOT published. Do not treat `url` as a live public docs host.
 * OpenAPI Scalar plugin is disabled until a Zarzour-owned OpenAPI document is available
 * (do not fetch stoat.chat openapi.json at runtime).
 */
const config: Config = {
  title: 'Zarzour Developers',
  tagline: 'Developer documentation for Zarzour (derived from Stoat)',
  // Official square dark-app favicon; do not hotlink upstream.
  favicon: '/img/favicon.ico',

  future: {
    v4: true,
  },

  // Unpublished local site only — not a verified public docs hostname.
  url: 'http://127.0.0.1:3000',
  baseUrl: '/',

  organizationName: 'zrzur',
  projectName: 'backend',

  onBrokenLinks: 'throw',

  i18n: {
    defaultLocale: 'en',
    locales: ['en'],
  },

  presets: [
    [
      'classic',
      {
        docs: {
          routeBasePath: '/',
          sidebarPath: './sidebars.ts',
          // Planned public edit URL when zrzur/backend is published (pending).
          editUrl: undefined,
        },
      } satisfies Preset.Options,
    ],
  ],

  plugins: [
    [
      '@docusaurus/plugin-client-redirects',
      {
        fromExtensions: ['html', 'htm'],
        redirects: [
          {
            from: '/developers/api/reference.html',
            to: '/developers/endpoints',
          },
          {
            from: '/contrib.html',
            to: '/developing/contrib',
          },
          {
            from: '/contrib',
            to: '/developing/contrib',
          },
        ],
      },
    ],
  ],

  themeConfig: {
    colorMode: {
      respectPrefersColorScheme: true,
    },
    navbar: {
      logo: {
        alt: 'Zarzour for Developers',
        src: '/img/navbar.light.svg',
        srcDark: '/img/navbar.dark.svg',
      },
      items: [
        {
          type: 'doc',
          docId: 'index',
          label: 'Docs',
        },
        {
          href: 'https://github.com/zrzur',
          label: 'GitHub',
          position: 'right',
        },
      ],
    },
    footer: {
      style: 'dark',
      links: [
        {
          title: 'Developers',
          items: [
            {
              label: 'Source (planned org)',
              href: 'https://github.com/zrzur',
            },
            {
              label: 'Upstream Stoat (attribution)',
              href: 'https://github.com/stoatchat/stoatchat',
            },
          ],
        },
        {
          title: 'Product',
          items: [
            {
              label: 'zarzour.cc',
              href: 'https://zarzour.cc',
            },
          ],
        },
      ],
      copyright: `Zarzour documentation (local). Upstream Stoat/Revolt AGPL copyrights preserved in repository LICENSE files. © ${new Date().getFullYear()}`,
    },
    prism: {
      theme: prismThemes.github,
      darkTheme: prismThemes.dracula,
    },
  } satisfies Preset.ThemeConfig,
};

export default config;
