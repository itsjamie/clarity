import { ScreenCaptureManager } from './screen-capture-manager';

describe('ScreenCaptureManager', () => {
  const originalSecureContext = Object.getOwnPropertyDescriptor(window, 'isSecureContext');
  const originalMediaDevices = Object.getOwnPropertyDescriptor(navigator, 'mediaDevices');

  afterEach(() => {
    restoreProperty(window, 'isSecureContext', originalSecureContext);
    restoreProperty(navigator, 'mediaDevices', originalMediaDevices);
  });

  it('prefers window audio when shared audio is requested', async () => {
    const getDisplayMedia = installDisplayCapture(createCaptureStream(true));
    const manager = new ScreenCaptureManager(vi.fn());

    await manager.start('text', true);

    expect(getDisplayMedia).toHaveBeenCalledWith(
      expect.objectContaining({
        audio: true,
        windowAudio: 'window',
      }),
    );
    expect(getDisplayMedia.mock.calls[0]?.[0]).not.toHaveProperty('systemAudio');
  });

  it('excludes window audio when shared audio is disabled', async () => {
    const getDisplayMedia = installDisplayCapture(createCaptureStream(false));
    const manager = new ScreenCaptureManager(vi.fn());

    await manager.start('text', false);

    expect(getDisplayMedia).toHaveBeenCalledWith(
      expect.objectContaining({
        audio: false,
        windowAudio: 'exclude',
      }),
    );
    expect(getDisplayMedia.mock.calls[0]?.[0]).not.toHaveProperty('systemAudio');
  });
});

function installDisplayCapture(stream: MediaStream) {
  const getDisplayMedia = vi.fn().mockResolvedValue(stream);
  Object.defineProperty(window, 'isSecureContext', {
    configurable: true,
    value: true,
  });
  Object.defineProperty(navigator, 'mediaDevices', {
    configurable: true,
    value: { getDisplayMedia },
  });
  return getDisplayMedia;
}

function createCaptureStream(withAudio: boolean): MediaStream {
  const videoTrack = {
    contentHint: '',
    getSettings: () => ({
      width: 1920,
      height: 1080,
      frameRate: 30,
      displaySurface: 'window',
    }),
    addEventListener: vi.fn(),
    stop: vi.fn(),
  } as unknown as MediaStreamTrack;
  const audioTrack = { stop: vi.fn() } as unknown as MediaStreamTrack;
  const audioTracks = withAudio ? [audioTrack] : [];

  return {
    getVideoTracks: () => [videoTrack],
    getAudioTracks: () => audioTracks,
    getTracks: () => [videoTrack, ...audioTracks],
  } as unknown as MediaStream;
}

function restoreProperty(
  target: Window | Navigator,
  property: 'isSecureContext' | 'mediaDevices',
  descriptor: PropertyDescriptor | undefined,
): void {
  if (descriptor) {
    Object.defineProperty(target, property, descriptor);
  } else {
    Reflect.deleteProperty(target, property);
  }
}
