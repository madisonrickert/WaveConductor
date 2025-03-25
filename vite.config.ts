import { defineConfig } from 'vite';
import react from '@vitejs/plugin-react';
import glsl from 'vite-plugin-glsl';
import eslint from 'vite-plugin-eslint';

export default defineConfig({
  plugins: [react(), glsl(), eslint({})],
  build: {
    outDir: 'public',
  },
});
