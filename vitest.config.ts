/// <reference types="vitest/config" />
import { defineConfig, Plugin } from 'vitest/config';
import react from '@vitejs/plugin-react';
import path from 'path';

// Stub GLSL imports (vite-plugin-glsl is a build-time plugin, not needed in tests)
function glslStub(): Plugin {
  return {
    name: 'glsl-stub',
    transform(_code: string, id: string) {
      if (/\.(glsl|vert|frag)(\\?|$)/.test(id)) {
        return { code: 'export default "";', map: null };
      }
    },
  };
}

export default defineConfig({
  plugins: [react(), glslStub()],
  resolve: {
    alias: {
      '@': path.resolve(__dirname, 'src'),
    },
  },
  test: {
    globals: true,
    environment: 'jsdom',
    setupFiles: ['./src/test/setup.ts'],
    include: ['src/**/*.test.{ts,tsx}'],
    coverage: {
      provider: 'v8',
      include: ['src/**/*.{ts,tsx}'],
      exclude: [
        'src/types/**',
        'src/test/**',
        'src/**/*.d.ts',
      ],
    },
  },
});
