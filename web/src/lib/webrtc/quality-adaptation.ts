import type {
  CaptureMode,
  EncodingProfile,
  QualityStrategy,
} from './profiles';
import { profilesForMode } from './profiles';

export interface AdaptationSample {
  packetLossRatio?: number;
  roundTripTimeMs?: number;
  availableOutgoingBitrate?: number;
  framesPerSecond?: number;
  qualityLimitationReason?: 'none' | 'cpu' | 'bandwidth' | 'other';
  bitrate?: number;
}

export interface AdaptationDecision {
  profile: EncodingProfile;
  changed: boolean;
  reason: string;
  limitation: 'none' | 'cpu' | 'bandwidth' | 'network';
}

export const ADAPTATION_POLICY = {
  unhealthySamplesToDegrade: 3,
  healthySamplesToUpgrade: 15,
  degradeCooldownMs: 10_000,
  upgradeCooldownMs: 30_000,
  highPacketLossRatio: 0.05,
  healthyPacketLossRatio: 0.01,
  highRoundTripTimeMs: 500,
  healthyRoundTripTimeMs: 250,
  targetBitrateHeadroom: 1.15,
  lowBitrateRatio: 0.7,
  smoothingFactor: 0.35,
} as const;

export class QualityAdaptationController {
  readonly #mode: CaptureMode;
  #strategy: QualityStrategy;
  #profileIndex: number;
  #unhealthySamples = 0;
  #healthySamples = 0;
  #lastChangeAt = Number.NEGATIVE_INFINITY;
  #averageLoss: number | undefined;
  #averageRtt: number | undefined;

  public constructor(
    mode: CaptureMode,
    strategy: QualityStrategy = 'adaptive',
    initialProfileIndex = 0,
  ) {
    this.#mode = mode;
    this.#strategy = strategy;
    this.#profileIndex = Math.max(
      0,
      Math.min(initialProfileIndex, profilesForMode(mode).length - 1),
    );
  }

  public get profile(): EncodingProfile {
    const profile = profilesForMode(this.#mode)[this.#profileIndex];
    if (!profile) throw new Error('Adaptation profile is unavailable.');
    return profile;
  }

  public clone(): QualityAdaptationController {
    const copy = new QualityAdaptationController(
      this.#mode,
      this.#strategy,
      this.#profileIndex,
    );
    copy.#unhealthySamples = this.#unhealthySamples;
    copy.#healthySamples = this.#healthySamples;
    copy.#lastChangeAt = this.#lastChangeAt;
    copy.#averageLoss = this.#averageLoss;
    copy.#averageRtt = this.#averageRtt;
    return copy;
  }

  public setStrategy(strategy: QualityStrategy): void {
    this.#strategy = strategy;
    this.#unhealthySamples = 0;
    this.#healthySamples = 0;
  }

  public setFixedProfile(index: number): EncodingProfile {
    this.#profileIndex = Math.max(0, Math.min(index, profilesForMode(this.#mode).length - 1));
    return this.profile;
  }

  public reset(): void {
    this.#unhealthySamples = 0;
    this.#healthySamples = 0;
    this.#averageLoss = undefined;
    this.#averageRtt = undefined;
  }

  public evaluate(sample: AdaptationSample, nowMs: number): AdaptationDecision {
    this.#averageLoss = smooth(this.#averageLoss, sample.packetLossRatio);
    this.#averageRtt = smooth(this.#averageRtt, sample.roundTripTimeMs);
    const limitation = classifyLimitation(sample);
    if (this.#strategy === 'fixed') {
      return { profile: this.profile, changed: false, reason: 'Fixed quality selected.', limitation };
    }

    const unhealthy = this.#isUnhealthy(sample);
    const healthy = this.#isHealthy(sample);
    this.#unhealthySamples = unhealthy ? this.#unhealthySamples + 1 : 0;
    this.#healthySamples = healthy ? this.#healthySamples + 1 : 0;

    if (
      this.#unhealthySamples >= ADAPTATION_POLICY.unhealthySamplesToDegrade &&
      this.#profileIndex < profilesForMode(this.#mode).length - 1 &&
      nowMs - this.#lastChangeAt >= ADAPTATION_POLICY.degradeCooldownMs
    ) {
      this.#profileIndex += 1;
      this.#lastChangeAt = nowMs;
      this.#unhealthySamples = 0;
      this.#healthySamples = 0;
      return {
        profile: this.profile,
        changed: true,
        reason: degradationReason(limitation),
        limitation,
      };
    }

    if (
      this.#healthySamples >= ADAPTATION_POLICY.healthySamplesToUpgrade &&
      this.#profileIndex > 0 &&
      nowMs - this.#lastChangeAt >= ADAPTATION_POLICY.upgradeCooldownMs
    ) {
      const next = profilesForMode(this.#mode)[this.#profileIndex - 1];
      if (next && (sample.availableOutgoingBitrate ?? Number.POSITIVE_INFINITY) >= next.maxBitrate * ADAPTATION_POLICY.targetBitrateHeadroom) {
        this.#profileIndex -= 1;
        this.#lastChangeAt = nowMs;
        this.#healthySamples = 0;
        return {
          profile: this.profile,
          changed: true,
          reason: 'Connection remained healthy long enough to raise quality.',
          limitation: 'none',
        };
      }
    }

    return {
      profile: this.profile,
      changed: false,
      reason: unhealthy ? 'Monitoring sustained connection pressure.' : 'Quality is stable.',
      limitation,
    };
  }

  #isUnhealthy(sample: AdaptationSample): boolean {
    return (
      (this.#averageLoss ?? 0) > ADAPTATION_POLICY.highPacketLossRatio ||
      (this.#averageRtt ?? 0) > ADAPTATION_POLICY.highRoundTripTimeMs ||
      sample.qualityLimitationReason === 'bandwidth' ||
      sample.qualityLimitationReason === 'cpu' ||
      (sample.availableOutgoingBitrate !== undefined &&
        sample.availableOutgoingBitrate < this.profile.maxBitrate * ADAPTATION_POLICY.lowBitrateRatio)
    );
  }

  #isHealthy(sample: AdaptationSample): boolean {
    return (
      (this.#averageLoss === undefined || this.#averageLoss < ADAPTATION_POLICY.healthyPacketLossRatio) &&
      (this.#averageRtt === undefined || this.#averageRtt < ADAPTATION_POLICY.healthyRoundTripTimeMs) &&
      (sample.qualityLimitationReason === undefined || sample.qualityLimitationReason === 'none')
    );
  }
}

function smooth(previous: number | undefined, current: number | undefined): number | undefined {
  if (current === undefined) return previous;
  if (previous === undefined) return current;
  return previous + ADAPTATION_POLICY.smoothingFactor * (current - previous);
}

function classifyLimitation(sample: AdaptationSample): AdaptationDecision['limitation'] {
  if (sample.qualityLimitationReason === 'cpu') return 'cpu';
  if (sample.qualityLimitationReason === 'bandwidth') return 'bandwidth';
  if (
    (sample.packetLossRatio ?? 0) > ADAPTATION_POLICY.highPacketLossRatio ||
    (sample.roundTripTimeMs ?? 0) > ADAPTATION_POLICY.highRoundTripTimeMs
  ) return 'network';
  return 'none';
}

function degradationReason(limitation: AdaptationDecision['limitation']): string {
  switch (limitation) {
    case 'cpu': return 'CPU limited. Reduced this viewer’s sender profile.';
    case 'bandwidth': return 'Bandwidth limited. Reduced this viewer’s sender profile.';
    case 'network': return 'Sustained network pressure. Reduced this viewer’s sender profile.';
    case 'none': return 'Sustained encoder pressure. Reduced this viewer’s sender profile.';
  }
}
