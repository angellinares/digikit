// The UI preview: the desktop pages in a plain browser, over a fake device and a fake samples folder, without the Tauri app.
// Never part of the app build (`vite build ui` has its own root and config).
//   mise run ui:preview            (serves http://127.0.0.1:5199/)
// The Tauri `invoke` and the two backends are replaced by modules of this folder; everything else is the app's own code.
import { fileURLToPath } from 'node:url';
import { defineConfig } from 'vite';
import solid from 'vite-plugin-solid';

const here = (path: string) => fileURLToPath(new URL(path, import.meta.url));

export default defineConfig({
  root: here('.'),
  base: './',
  plugins: [solid()],
  resolve: {
    alias: [
      { find: '@tauri-apps/api/core', replacement: here('./tauri-mock.ts') },
      // the pages import their backends by these two relative names; the mocks answer instead
      { find: /^\.\/transfer-backend$/, replacement: here('./transfer-backend.mock.ts') },
      { find: /^\.\/memory-backend$/, replacement: here('./memory-backend.mock.ts') },
      { find: /^\.\/profile-backend$/, replacement: here('./profile-backend.mock.ts') },
      { find: /^\.\/samples-backend$/, replacement: here('./samples-backend.mock.ts') },
    ],
    dedupe: ['solid-js'],
  },
  server: { host: '127.0.0.1', port: 5199, strictPort: true, fs: { allow: [here('../../../..')] } },
});
