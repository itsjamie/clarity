import { expect, test } from '@playwright/test';

test.describe('forced TURN relay', () => {
  test.skip(!process.env.CLARITY_FORCE_RELAY, 'Run with the documented coturn integration stack.');

  test('selects a relay candidate and carries video with temporary credentials', async ({ browser, page, baseURL }) => {
    await page.addInitScript(() => {
      window.sessionStorage.setItem('clarity:test:synthetic-capture', 'enabled');
      window.sessionStorage.setItem('clarity:test:force-relay', 'enabled');
    });
    await page.goto('/welcome');
    await page.getByRole('button', { name: 'Open room' }).click();
    await expect(page.getByText('Ready to share', { exact: true })).toBeVisible();
    await page.getByRole('button', { name: 'Share my screen' }).click();
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
    await viewer.waitForLoadState('domcontentloaded');
    const nameField = viewer.getByLabel('Display name optional');
    if (await nameField.count()) {
      await nameField.fill('Relay Viewer');
      await viewer.getByRole('button', { name: 'Join room' }).click();
    }

    await expect.poll(async () => viewer.locator('video').evaluate((video) => {
      const element = video as HTMLVideoElement;
      return element.readyState >= HTMLMediaElement.HAVE_CURRENT_DATA && element.videoWidth > 0;
    }), { timeout: 25_000 }).toBe(true);
    await viewer.locator('.viewer-controls-zone').hover();
    await viewer.getByRole('button', { name: 'Diagnostics' }).click();
    await expect(viewer.locator('.room-diag__badge--relay')).toHaveText('relay', { timeout: 15_000 });
    await page.getByRole('tab', { name: 'Room', exact: true }).click();
    const relayViewer = page.locator('.peer-card').first();
    await relayViewer.getByText('Connection details', { exact: true }).click();
    await expect(relayViewer.getByText('TURN relay', { exact: true })).toBeVisible();

    await viewerContext.close();
  });
});
