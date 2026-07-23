import { fileURLToPath, URL } from 'node:url';
import { defineConfig } from 'vitest/config';
import react from '@vitejs/plugin-react';

const developmentServer = process.env.CLARITY_DEV_SERVER_ORIGIN ?? 'http://127.0.0.1:3000';
const developmentWebPort = Number(process.env.CLARITY_DEV_WEB_PORT ?? '5173');

export default defineConfig({
  plugins: [react()],
  resolve: {
    alias: { '@': fileURLToPath(new URL('./src', import.meta.url)) },
  },
  server: {
    host: '127.0.0.1',
    port: developmentWebPort,
    proxy: {
      '/api': { target: developmentServer, ws: true },
      '/healthz': { target: developmentServer },
      '/readyz': { target: developmentServer },
    },
  },
  build: {
    sourcemap: false,
    target: 'es2022',
  },
  test: {
    globals: true,
    environment: 'jsdom',
    setupFiles: ['./src/testing/setup-tests.ts'],
    include: ['src/**/*.test.{ts,tsx}'],
    coverage: { reporter: ['text', 'html'] },
  },
});
