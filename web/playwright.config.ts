import { defineConfig, devices } from '@playwright/test';

const testServerPort = process.env.CLARITY_TEST_SERVER_PORT ?? '3000';
const testServerOrigin = `http://127.0.0.1:${testServerPort}`;
const testWebPort = process.env.CLARITY_TEST_WEB_PORT ?? '5173';
const testWebOrigin = `http://127.0.0.1:${testWebPort}`;

export default defineConfig({
  testDir: './tests',
  fullyParallel: false,
  forbidOnly: Boolean(process.env.CI),
  retries: process.env.CI ? 2 : 0,
  workers: 1,
  reporter: [['list'], ['html', { open: 'never' }]],
  use: {
    baseURL: testWebOrigin,
    trace: 'on-first-retry',
    video: 'retain-on-failure',
    permissions: [],
  },
  projects: [{ name: 'chromium', use: { ...devices['Desktop Chrome'] } }],
  webServer: [
    {
      command: 'cargo run -p clarity-server',
      cwd: '..',
      url: `${testServerOrigin}/readyz`,
      reuseExistingServer: !process.env.CI,
      timeout: 120_000,
      env: {
        ...process.env,
        APP_ENV: 'development',
        APP_BIND_ADDRESS: `127.0.0.1:${testServerPort}`,
        PUBLIC_BASE_URL: testServerOrigin,
        ALLOWED_ORIGINS: testWebOrigin,
        AUTH_RATE_LIMIT: '100',
      },
    },
    {
      command: 'pnpm dev:test',
      url: testWebOrigin,
      reuseExistingServer: !process.env.CI,
      timeout: 120_000,
      env: {
        ...process.env,
        CLARITY_DEV_SERVER_ORIGIN: testServerOrigin,
        CLARITY_DEV_WEB_PORT: testWebPort,
      },
    },
  ],
});
