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

    await manager.start('text', true, '1440p');

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

    await manager.start('text', false, '1440p');

    expect(getDisplayMedia).toHaveBeenCalledWith(
      expect.objectContaining({
        audio: false,
        windowAudio: 'exclude',
      }),
    );
    expect(getDisplayMedia.mock.calls[0]?.[0]).not.toHaveProperty('systemAudio');
  });

  it.each([
    ['1440p', 2560, 1440],
    ['4k', 3840, 2160],
  ] as const)('requests the %s capture target on the native track', async (resolution, width, height) => {
    const getDisplayMedia = installDisplayCapture(createCaptureStream(false));
    const manager = new ScreenCaptureManager(vi.fn());

    await manager.start('text', false, resolution);

    const options = getDisplayMedia.mock.calls[0]?.[0];
    expect(options?.video).toEqual(expect.objectContaining({
      width: { ideal: width, max: width },
      height: { ideal: height, max: height },
    }));
  });

  it.each([
    ['text', 30],
    ['motion', 60],
  ] as const)('caps %s capture at %i FPS before encoding', async (mode, frameRate) => {
    const getDisplayMedia = installDisplayCapture(createCaptureStream(false));
    const manager = new ScreenCaptureManager(vi.fn());

    await manager.start(mode, false, '1440p');

    const options = getDisplayMedia.mock.calls[0]?.[0];
    expect(options?.video).toEqual(expect.objectContaining({
      frameRate: { ideal: frameRate, max: frameRate },
    }));
  });
});

function installDisplayCapture(stream: MediaStream) {
  const getDisplayMedia = vi
    .fn<(options: DisplayMediaStreamOptions) => Promise<MediaStream>>()
    .mockResolvedValue(stream);
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
