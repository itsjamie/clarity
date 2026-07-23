import { CodecCapabilityService } from './codec-capability-service';

type CodecCapability = NonNullable<ReturnType<typeof RTCRtpSender.getCapabilities>>['codecs'][number];

const codecs: CodecCapability[] = [
  { mimeType: 'video/VP8', clockRate: 90_000 },
  { mimeType: 'video/rtx', clockRate: 90_000 },
  { mimeType: 'video/H264', clockRate: 90_000, sdpFmtpLine: 'profile-level-id=42e01f' },
  { mimeType: 'video/AV1', clockRate: 90_000, sdpFmtpLine: 'profile=0' },
  { mimeType: 'video/VP9', clockRate: 90_000, sdpFmtpLine: 'profile-id=0' },
  { mimeType: 'video/red', clockRate: 90_000 },
];

describe('automatic codec preference', () => {
  afterEach(() => {
    vi.unstubAllGlobals();
    Reflect.deleteProperty(navigator, 'mediaCapabilities');
  });

  it('prefers smooth modern codecs even when software encoding is not power efficient', async () => {
    vi.stubGlobal('RTCRtpSender', {
      getCapabilities: () => ({ codecs, headerExtensions: [] }),
    });
    Object.defineProperty(navigator, 'mediaCapabilities', {
      configurable: true,
      value: {
        encodingInfo: vi.fn().mockResolvedValue({
          supported: true,
          smooth: true,
          powerEfficient: false,
        }),
      },
    });
    const setCodecPreferences = vi.fn<(preference: CodecCapability[]) => void>();
    const transceiver = { setCodecPreferences } as unknown as RTCRtpTransceiver;

    const result = await new CodecCapabilityService().applyPreference(transceiver, 'auto');

    expect(result).toEqual({ applied: true, selected: 'AV1' });
    expect(setCodecPreferences).toHaveBeenCalledOnce();
    const preference = setCodecPreferences.mock.calls[0]?.[0] ?? [];
    expect(preference.map((codec) => codec.mimeType)).toEqual([
      'video/AV1',
      'video/VP9',
      'video/H264',
      'video/VP8',
      'video/rtx',
      'video/red',
    ]);
  });
});
