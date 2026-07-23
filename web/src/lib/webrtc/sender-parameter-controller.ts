import type { EncodingProfile } from './profiles';

export interface SenderParameterResult {
  applied: Array<'maxBitrate' | 'maxFramerate' | 'scaleResolutionDownBy'>;
  unsupported: Array<'maxBitrate' | 'maxFramerate' | 'scaleResolutionDownBy'>;
}

type EncodingKey = SenderParameterResult['applied'][number];

export class SenderParameterController {
  public async apply(
    sender: RTCRtpSender,
    profile: EncodingProfile,
    mode: 'text' | 'motion',
  ): Promise<SenderParameterResult> {
    const parameters = sender.getParameters();
    if (parameters.encodings.length === 0) parameters.encodings = [{}];
    parameters.degradationPreference =
      mode === 'text' ? 'maintain-resolution' : 'maintain-framerate';
    const requested: Record<EncodingKey, number> = {
      maxBitrate: profile.maxBitrate,
      maxFramerate: profile.maxFramerate,
      scaleResolutionDownBy: profile.scaleResolutionDownBy,
    };
    const applied: EncodingKey[] = [];
    const unsupported: EncodingKey[] = [];

    for (const key of Object.keys(requested) as EncodingKey[]) {
      const candidate = sender.getParameters();
      if (candidate.encodings.length === 0) candidate.encodings = [{}];
      const encoding = candidate.encodings[0];
      if (!encoding) {
        unsupported.push(key);
        continue;
      }
      encoding[key] = requested[key];
      if (key === 'maxBitrate') candidate.degradationPreference = parameters.degradationPreference;
      try {
        await sender.setParameters(candidate);
        applied.push(key);
      } catch {
        unsupported.push(key);
      }
    }
    return { applied, unsupported };
  }
}
