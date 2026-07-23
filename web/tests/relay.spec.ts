import { expect, test } from '@playwright/test';

test.describe('forced TURN relay', () => {
  test.skip(!process.env.CLARITY_FORCE_RELAY, 'Run with the documented coturn integration stack.');

  test('selects a relay candidate and carries video with temporary credentials', async ({ browser, page, baseURL }) => {
    await page.addInitScript(() => {
      window.sessionStorage.setItem('clarity:test:synthetic-capture', 'enabled');
      window.sessionStorage.setItem('clarity:test:force-relay', 'enabled');
    });
    await page.goto('/');
    await page.getByRole('button', { name: 'Create room' }).click();
    await expect(page.getByText('Ready to share', { exact: true })).toBeVisible();
    await page.getByRole('button', { name: 'Start sharing' }).click();
    await expect(page.getByText("You're sharing your screen", { exact: true })).toBeVisible();

    const roomId = new URL(page.url()).pathname.split('/').at(-1);
    if (!roomId) throw new Error('Presenter room id is missing.');
    const storedInvite = await page.evaluate((id) =>
      window.sessionStorage.getItem(`clarity:room:${id}:viewer-url`), roomId);
    if (!storedInvite) throw new Error('Viewer invitation is missing.');
    const invite = new URL(storedInvite);
    const testOrigin = new URL(baseURL ?? 'http://127.0.0.1:5173');
    invite.protocol = testOrigin.protocol;
    invite.host = testOrigin.host;

    const viewerContext = await browser.newContext();
    await viewerContext.addInitScript(() => {
      window.sessionStorage.setItem('clarity:test:force-relay', 'enabled');
    });
    const viewer = await viewerContext.newPage();
    await viewer.goto(invite.toString());
    await viewer.getByLabel('Display name optional').fill('Relay Viewer');
    await viewer.getByRole('button', { name: 'Join room' }).click();

    await expect(viewer.getByText('live', { exact: true })).toBeVisible({ timeout: 25_000 });
    await expect.poll(async () => viewer.locator('video').evaluate((video) => {
      const element = video as HTMLVideoElement;
      return element.readyState >= HTMLMediaElement.HAVE_CURRENT_DATA && element.videoWidth > 0;
    }), { timeout: 25_000 }).toBe(true);
    await expect(viewer.getByText('TURN relay', { exact: true })).toBeVisible({ timeout: 15_000 });
    const relayViewer = page.locator('.peer-card').filter({ hasText: 'Relay Viewer' });
    await relayViewer.getByText('Connection details', { exact: true }).click();
    await expect(relayViewer.getByText('TURN relay', { exact: true })).toBeVisible();

    await viewerContext.close();
  });
});
