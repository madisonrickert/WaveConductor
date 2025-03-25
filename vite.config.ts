import { defineConfig } from 'vite';
import react from '@vitejs/plugin-react';
import glsl from 'vite-plugin-glsl';
import eslint from 'vite-plugin-eslint';

// Required for leapjs 0.6.4
import { nodePolyfills } from 'vite-plugin-node-polyfills';

export default defineConfig({
  plugins: [react(), glsl(), nodePolyfills(), eslint({})],
  build: {
    outDir: 'public',
  },
});
