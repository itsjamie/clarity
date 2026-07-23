export type CodecMode = 'auto' | 'AV1' | 'VP9' | 'H264' | 'VP8';

const MIME_BY_MODE: Readonly<Record<Exclude<CodecMode, 'auto'>, string>> = {
  AV1: 'video/AV1',
  VP9: 'video/VP9',
  H264: 'video/H264',
  VP8: 'video/VP8',
};

const QUALITY_ORDER = ['AV1', 'VP9', 'H264', 'VP8'] as const;

type SenderCapabilities = NonNullable<ReturnType<typeof RTCRtpSender.getCapabilities>>;
type CodecCapability = SenderCapabilities['codecs'][number];

export class CodecCapabilityService {
  public supportedModes(): CodecMode[] {
    const capabilities = RTCRtpSender.getCapabilities?.('video');
    if (!capabilities) return ['auto'];
    const mimeTypes = new Set(capabilities.codecs.map((codec) => codec.mimeType.toLowerCase()));
    return [
      'auto',
      ...(['AV1', 'VP9', 'H264', 'VP8'] as const).filter((mode) =>
        mimeTypes.has(MIME_BY_MODE[mode].toLowerCase()),
      ),
    ];
  }

  public async applyPreference(
    transceiver: RTCRtpTransceiver,
    mode: CodecMode,
  ): Promise<{ applied: boolean; selected: CodecMode }> {
    if (typeof transceiver.setCodecPreferences !== 'function') {
      return { applied: false, selected: mode };
    }
    const capabilities = RTCRtpSender.getCapabilities?.('video');
    if (!capabilities) return { applied: false, selected: mode };
    const preference = mode === 'auto'
      ? await this.#automaticPreferences(capabilities.codecs)
      : explicitPreferences(capabilities.codecs, mode);
    const selected = firstPrimaryMode(preference) ?? mode;
    if (preference.length === 0 || selected === 'auto') return { applied: false, selected };
    transceiver.setCodecPreferences(preference);
    return { applied: true, selected };
  }

  async #automaticPreferences(codecs: readonly CodecCapability[]): Promise<CodecCapability[]> {
    const supported = new Set(codecs.map((codec) => codec.mimeType.toLowerCase()));
    const smoothModern: Exclude<CodecMode, 'auto'>[] = [];
    for (const mode of ['AV1', 'VP9'] as const) {
      if (!supported.has(MIME_BY_MODE[mode].toLowerCase())) continue;
      if (await encodingShouldBePreferred(MIME_BY_MODE[mode])) smoothModern.push(mode);
    }
    const legacyModes = ['H264', 'VP8'] as const;
    const deferredModern = (['AV1', 'VP9'] as const).filter(
      (mode) => !smoothModern.includes(mode),
    );
    const orderedModes = [...smoothModern, ...legacyModes, ...deferredModern];
    const rank = new Map(orderedModes.map((mode, index) => [MIME_BY_MODE[mode].toLowerCase(), index]));

    return codecs
      .map((codec, originalIndex) => ({ codec, originalIndex }))
      .sort((left, right) => {
        const leftRank = rank.get(left.codec.mimeType.toLowerCase()) ?? orderedModes.length;
        const rightRank = rank.get(right.codec.mimeType.toLowerCase()) ?? orderedModes.length;
        return leftRank - rightRank || left.originalIndex - right.originalIndex;
      })
      .map(({ codec }) => codec);
  }
}

function explicitPreferences(
  codecs: readonly CodecCapability[],
  mode: Exclude<CodecMode, 'auto'>,
): CodecCapability[] {
  const mime = MIME_BY_MODE[mode].toLowerCase();
  const preferred = codecs.filter((codec) => codec.mimeType.toLowerCase() === mime);
  if (preferred.length === 0) return [];
  const remaining = codecs.filter((codec) => codec.mimeType.toLowerCase() !== mime);
  return [...preferred, ...remaining];
}

function firstPrimaryMode(codecs: readonly CodecCapability[]): CodecMode | undefined {
  const mime = codecs.find((codec) => modeForMime(codec.mimeType) !== undefined)?.mimeType;
  return mime ? modeForMime(mime) : undefined;
}

function modeForMime(mimeType: string): Exclude<CodecMode, 'auto'> | undefined {
  const normalized = mimeType.toLowerCase();
  return QUALITY_ORDER.find((mode) => MIME_BY_MODE[mode].toLowerCase() === normalized);
}

interface WebRtcMediaCapabilities {
  encodingInfo(configuration: {
    type: 'webrtc';
    video: {
      contentType: string;
      width: number;
      height: number;
      bitrate: number;
      framerate: number;
    };
  }): Promise<{ supported: boolean; smooth: boolean; powerEfficient: boolean }>;
}

async function encodingShouldBePreferred(contentType: string): Promise<boolean> {
  const capabilities = navigator.mediaCapabilities as unknown as Partial<WebRtcMediaCapabilities>;
  // Sender capabilities are the authoritative support signal. Media Capabilities is only an
  // additional smoothness hint; lack of that optional API must not force a legacy codec.
  if (typeof capabilities.encodingInfo !== 'function') return true;
  try {
    const result = await capabilities.encodingInfo({
      type: 'webrtc',
      video: { contentType, width: 2560, height: 1440, bitrate: 18_000_000, framerate: 30 },
    });
    return result.supported && result.smooth;
  } catch {
    return true;
  }
}
