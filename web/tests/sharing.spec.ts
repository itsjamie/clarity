import { expect, test, type Browser, type BrowserContext, type Locator, type Page } from '@playwright/test';

interface ViewerHarness {
  context: BrowserContext;
  page: Page;
  name: string;
}

test.describe('Clarity Share browser mesh', () => {
  test('defaults to public admission and connects an invited viewer without approval', async ({ browser, page, baseURL }) => {
    await enableSyntheticCapture(page);
    await page.goto('/');
    await expect(page.getByRole('button', { name: /Public link/u })).toHaveAttribute('aria-pressed', 'true');
    const creationLimit = page.getByRole('spinbutton', { name: /Viewer limit/u });
    await expect(creationLimit).toHaveValue('10');
    await expect(creationLimit).toBeDisabled();
    await page.getByRole('button', { name: 'Create room' }).click();
    await expect(page.getByText('Ready to share', { exact: true })).toBeVisible();
    await expect(page.getByText(/Public link · Expires in/u)).toBeVisible();
    await page.getByRole('button', { name: 'Start sharing' }).click();

    const toolbarBounds = await page.locator('.presenter-stage__toolbar').boundingBox();
    const changeSourceBounds = await page.getByRole('button', { name: 'Change source' }).boundingBox();
    const codecBounds = await page.getByLabel('Codec').boundingBox();
    expect(toolbarBounds).not.toBeNull();
    expect(changeSourceBounds).not.toBeNull();
    expect(codecBounds).not.toBeNull();
    expect(changeSourceBounds!.x + changeSourceBounds!.width)
      .toBeLessThanOrEqual(toolbarBounds!.x + toolbarBounds!.width + 0.5);
    expect(Math.abs(changeSourceBounds!.height - codecBounds!.height)).toBeLessThanOrEqual(1);

    const invite = await viewerInvite(page, baseURL ?? 'http://127.0.0.1:5173');
    const viewer = await joinViewer(browser, invite, 'Public Viewer', false);
    await expectViewerLive(viewer.page);
    await expect(page.getByLabel('Room limit')).toHaveValue('10');
    await expect(page.getByRole('heading', { name: "Who's watching" })).toBeVisible();
    await expect(page.getByText('1 active', { exact: true })).toBeVisible();
    const publicViewerCard = page.locator('.peer-card').first();
    await expect(publicViewerCard).toBeVisible();
    await expect(publicViewerCard).toContainText('Anonymous viewer');
    await viewer.page.locator('.viewer-controls-zone').hover();
    await viewer.page.getByRole('button', { name: 'Set viewer name' }).click();
    await viewer.page.getByLabel('Name shown to presenter').fill('Public Viewer');
    await viewer.page.getByRole('button', { name: 'Save', exact: true }).click();
    await expect(viewer.page.getByRole('button', { name: 'Edit viewer name' })).toBeVisible();
    await expect(publicViewerCard).toContainText('Public Viewer');
    await expect(metricValue(publicViewerCard, 'Codec')).toContainText('video/AV1', { timeout: 10_000 });
    await expect(metricValue(publicViewerCard, 'Profile')).toHaveText('Text High');
    await expect(page.getByRole('img', { name: /Upload bitrate over the last 30 seconds/u })).toBeVisible();
    await expect(page.locator('.bitrate-chart__line')).toBeVisible();
    await expect(page.locator('.pending-list')).toHaveCount(0);
    await viewer.context.close();
  });

  test('approves independent viewers, pauses and resumes sharing, removes one, and ends cleanly', async ({ browser, page, baseURL }) => {
    await enableSyntheticCapture(page);
    await page.goto('/');
    await selectApprovalRequired(page);
    await page.getByRole('button', { name: 'Create room' }).click();
    await expect(page).toHaveURL(/\/present\//u);
    await expect(page.getByText('Ready to share', { exact: true })).toBeVisible();
    await page.getByRole('button', { name: 'Start sharing' }).click();
    await expect(page.getByText("You're sharing your screen", { exact: true })).toBeVisible();

    const invite = await viewerInvite(page, baseURL ?? 'http://127.0.0.1:5173');
    const first = await joinViewer(browser, invite, 'Ada Viewer');
    await approveViewer(page, 'Ada Viewer');
    await expectViewerLive(first.page);
    await expect(first.page).not.toHaveURL(/#/u);

    await first.page.locator('.viewer-controls-zone').hover();
    await expect(first.page.getByRole('toolbar', { name: 'Viewing controls' })).toBeVisible();
    await first.page.getByRole('button', { name: 'Fill', exact: true }).click();
    await expect(first.page.locator('.video-viewport')).toHaveClass(/video-viewport--fill/u);
    await first.page.getByRole('button', { name: '1:1' }).click();
    await expect(first.page.locator('.video-viewport')).toHaveClass(/video-viewport--pixel/u);
    await first.page.getByRole('button', { name: 'Zoom in' }).click();
    await expect(first.page.locator('.viewer-zoom-control output')).toHaveText('125%');
    await first.page.getByRole('button', { name: 'Fit', exact: true }).click();
    await first.page.getByRole('button', { name: 'Diagnostics' }).click();
    await expect(first.page.locator('.quality-hud')).toBeVisible();
    await expect(first.page.getByRole('button', { name: 'Enter fullscreen' })).toBeVisible();

    const second = await joinViewer(browser, invite, 'Grace Viewer');
    await approveViewer(page, 'Grace Viewer');
    await expectViewerLive(second.page);
    await expectViewerLive(first.page);

    await page.getByRole('button', { name: 'Change source' }).click();
    await expect(page.getByText("You're sharing your screen", { exact: true })).toBeVisible();
    await expectViewerLive(first.page);
    await expectViewerLive(second.page);

    await page.getByRole('button', { name: 'Stop sharing' }).click();
    await expect(page.locator('#presenter-stage-status')).toHaveText('Sharing paused');
    await expect(page.getByText('2 active', { exact: true })).toBeVisible();
    await expect(first.page.getByText('Sharing paused', { exact: true })).toBeVisible();
    await expect(second.page.getByText('Sharing paused', { exact: true })).toBeVisible();
    await expect(first.page.getByRole('heading', { name: 'The share has ended' })).toHaveCount(0);
    await expect(second.page.getByRole('heading', { name: 'The share has ended' })).toHaveCount(0);

    const late = await joinViewer(browser, invite, 'Late Viewer');
    await approveViewer(page, 'Late Viewer');
    await expect(late.page.getByText('Sharing paused', { exact: true })).toBeVisible();
    await expect(page.getByText('3 active', { exact: true })).toBeVisible();

    await page.getByRole('button', { name: 'Choose source to resume' }).click();
    await expect(page.getByText("You're sharing your screen", { exact: true })).toBeVisible();
    await expectViewerResolution(first.page, 1920, 1080);
    await expectViewerResolution(second.page, 1920, 1080);
    await expectViewerResolution(late.page, 1920, 1080);
    await expect(late.page.getByText('Sharing paused', { exact: true })).toHaveCount(0);

    const graceCard = page.locator('.peer-card').filter({ hasText: 'Grace Viewer' });
    await graceCard.getByRole('button', { name: 'Remove viewer' }).click();
    await expect(second.page.getByRole('heading', { name: 'You were removed' })).toBeVisible();
    await expectViewerLive(first.page);

    await page.getByRole('button', { name: 'End room' }).click();
    await page.getByRole('button', { name: 'End room now' }).click();
    await expect(first.page.getByRole('heading', { name: 'The share has ended' })).toBeVisible();
    await expect(late.page.getByRole('heading', { name: 'The share has ended' })).toBeVisible();

    await first.context.close();
    await second.context.close();
    await late.context.close();
  });

  test('keeps the invitation secret out of HTTP-visible URL components and blocks a fifth approval', async ({ browser, page, baseURL }) => {
    await enableSyntheticCapture(page);
    await page.goto('/');
    await selectApprovalRequired(page);
    await page.getByRole('button', { name: 'Create room' }).click();
    await expect(page.getByText('Ready to share', { exact: true })).toBeVisible();
    const invite = await viewerInvite(page, baseURL ?? 'http://127.0.0.1:5173');
    const parsedInvite = new URL(invite);
    expect(parsedInvite.search).toBe('');
    expect(parsedInvite.hash.length).toBeGreaterThan(20);

    const viewers: ViewerHarness[] = [];
    for (const name of ['One', 'Two', 'Three', 'Four']) {
      const viewer = await joinViewer(browser, invite, `Viewer ${name}`);
      viewers.push(viewer);
      await approveViewer(page, `Viewer ${name}`);
      await expect(viewer.page.getByText('Negotiating secure media…', { exact: true })).toBeVisible();
    }
    const fifth = await joinViewer(browser, invite, 'Viewer Five');
    viewers.push(fifth);
    const pendingRow = page.locator('.pending-list li').filter({ hasText: 'Viewer Five' });
    await expect(pendingRow.getByRole('button', { name: 'Approve' })).toBeDisabled();
    await expect(page.getByText('4 of 4 slots used', { exact: true })).toBeVisible();

    for (const viewer of viewers) await viewer.context.close();
  });
});

async function enableSyntheticCapture(page: Page): Promise<void> {
  await page.addInitScript(() => {
    window.sessionStorage.setItem('clarity:test:synthetic-capture', 'enabled');
  });
}

async function viewerInvite(presenter: Page, baseURL: string): Promise<string> {
  const roomId = new URL(presenter.url()).pathname.split('/').at(-1);
  if (!roomId) throw new Error('Presenter room id is missing.');
  const invite = await presenter.evaluate((id) =>
    window.sessionStorage.getItem(`clarity:room:${id}:viewer-url`), roomId);
  if (!invite) throw new Error('Viewer invitation is missing.');
  const parsed = new URL(invite);
  const testOrigin = new URL(baseURL);
  parsed.protocol = testOrigin.protocol;
  parsed.host = testOrigin.host;
  return parsed.toString();
}

async function joinViewer(
  browser: Browser,
  invite: string,
  name: string,
  requiresApproval = true,
): Promise<ViewerHarness> {
  const context = await browser.newContext();
  const page = await context.newPage();
  await page.goto(invite);
  await expect(page).not.toHaveURL(/#/u);
  if (requiresApproval) {
    await page.getByLabel('Display name optional').fill(name);
    await page.getByRole('button', { name: 'Join room' }).click();
    await expect(page.getByText('Awaiting approval', { exact: true })).toBeVisible();
  } else {
    await expect(page.getByLabel('Display name optional')).toHaveCount(0);
  }
  return { context, page, name };
}

async function selectApprovalRequired(page: Page): Promise<void> {
  await page.getByRole('button', { name: /Approval required/u }).click();
  await expect(page.getByRole('button', { name: /Approval required/u })).toHaveAttribute('aria-pressed', 'true');
  await expect(page.getByRole('spinbutton', { name: /Viewer limit/u })).toHaveValue('4');
  await expect(page.getByRole('spinbutton', { name: /Viewer limit/u })).toBeEnabled();
}

async function approveViewer(presenter: Page, name: string): Promise<void> {
  const row = presenter.locator('.pending-list li').filter({ hasText: name });
  await expect(row).toBeVisible();
  await row.getByRole('button', { name: 'Approve' }).click();
}

async function expectViewerLive(page: Page): Promise<void> {
  await expect.poll(async () => page.locator('video').evaluate((video) => {
    const element = video as HTMLVideoElement;
    return element.readyState >= HTMLMediaElement.HAVE_CURRENT_DATA && element.videoWidth > 0;
  })).toBe(true);
}

async function expectViewerResolution(page: Page, width: number, height: number): Promise<void> {
  await expect.poll(async () => page.locator('video').evaluate((video) => {
    const element = video as HTMLVideoElement;
    return [element.videoWidth, element.videoHeight];
  })).toEqual([width, height]);
}

function metricValue(card: Locator, label: string): Locator {
  return card.locator('.metric-grid > div').filter({ hasText: label }).locator('dd');
}
