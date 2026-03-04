import { defineConfig, Plugin } from 'vite';
import react from '@vitejs/plugin-react';
import glsl from 'vite-plugin-glsl';
import eslint from 'vite-plugin-eslint';
import path from 'path';

// Required for leapjs 0.6.4
import { nodePolyfills } from 'vite-plugin-node-polyfills';

function getElectronPlugins(): Plugin[] {
  if (!process.env.ELECTRON) return [];

  // eslint-disable-next-line @typescript-eslint/no-require-imports
  const electron = require('vite-plugin-electron').default;
  return electron([
    {
      entry: 'electron/main.ts',
      vite: { build: { outDir: 'dist-electron' } },
    },
    {
      entry: 'electron/preload.ts',
      vite: { build: { outDir: 'dist-electron' } },
    },
  ]);
}

export default defineConfig({
  base: process.env.ELECTRON ? './' : '/chargallery/',
  plugins: [react(), glsl(), nodePolyfills(), eslint({}), ...getElectronPlugins()],
  resolve: {
    alias: {
      '@': path.resolve(__dirname, 'src'),
    },
  },
  build: {
    outDir: 'dist',
    chunkSizeWarningLimit: 1000,
  },
});
