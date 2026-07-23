import { chromium } from '@playwright/test';

const target = process.argv.slice(2).find((value) => value !== '--') ?? 'http://127.0.0.1:3000/';
const browser = await chromium.launch();
const page = await browser.newPage();
const failures = [];

page.on('console', (message) => {
  if (message.type() === 'error') failures.push(`console: ${message.text()}`);
});
page.on('pageerror', (error) => failures.push(`page: ${error.message}`));
page.on('requestfailed', (request) => {
  failures.push(`request: ${request.url()} (${request.failure()?.errorText ?? 'unknown'})`);
});

try {
  const response = await page.goto(target, { waitUntil: 'networkidle' });
  if (!response?.ok()) failures.push(`navigation: HTTP ${response?.status() ?? 'unknown'}`);
  await page.locator('main h1').first().waitFor({ state: 'visible', timeout: 10_000 });
} catch (error) {
  failures.push(error instanceof Error ? error.message : String(error));
} finally {
  await browser.close();
}

if (failures.length > 0) {
  throw new Error(`Running-app smoke test failed:\n${failures.join('\n')}`);
}

console.log(`running-app smoke test passed: ${target}`);
