export type CaptureMode = 'text' | 'motion';
export type QualityStrategy = 'adaptive' | 'fixed';
export type ProfileLevel = 'high' | 'medium' | 'constrained' | 'low';

export interface EncodingProfile {
  id: `${CaptureMode}-${ProfileLevel}`;
  mode: CaptureMode;
  level: ProfileLevel;
  label: string;
  scaleResolutionDownBy: number;
  maxFramerate: number;
  maxBitrate: number;
}

export const TEXT_PROFILES: readonly EncodingProfile[] = [
  { id: 'text-high', mode: 'text', level: 'high', label: 'Text High', scaleResolutionDownBy: 1, maxFramerate: 30, maxBitrate: 18_000_000 },
  { id: 'text-medium', mode: 'text', level: 'medium', label: 'Text Medium', scaleResolutionDownBy: 1, maxFramerate: 24, maxBitrate: 12_000_000 },
  { id: 'text-constrained', mode: 'text', level: 'constrained', label: 'Text Constrained', scaleResolutionDownBy: 1, maxFramerate: 15, maxBitrate: 8_000_000 },
  { id: 'text-low', mode: 'text', level: 'low', label: 'Text Low', scaleResolutionDownBy: 1.5, maxFramerate: 15, maxBitrate: 5_000_000 },
] as const;

export const MOTION_PROFILES: readonly EncodingProfile[] = [
  { id: 'motion-high', mode: 'motion', level: 'high', label: 'Motion High', scaleResolutionDownBy: 1, maxFramerate: 60, maxBitrate: 24_000_000 },
  { id: 'motion-medium', mode: 'motion', level: 'medium', label: 'Motion Medium', scaleResolutionDownBy: 1.333, maxFramerate: 60, maxBitrate: 16_000_000 },
  { id: 'motion-constrained', mode: 'motion', level: 'constrained', label: 'Motion Constrained', scaleResolutionDownBy: 1.333, maxFramerate: 30, maxBitrate: 10_000_000 },
  { id: 'motion-low', mode: 'motion', level: 'low', label: 'Motion Low', scaleResolutionDownBy: 2, maxFramerate: 30, maxBitrate: 5_000_000 },
] as const;

export function profilesForMode(mode: CaptureMode): readonly EncodingProfile[] {
  return mode === 'text' ? TEXT_PROFILES : MOTION_PROFILES;
}

export function initialProfile(mode: CaptureMode): EncodingProfile {
  const profile = profilesForMode(mode)[0];
  if (!profile) throw new Error('No encoding profile is configured.');
  return profile;
}

export function estimatedUploadBitsPerSecond(
  profile: EncodingProfile,
  approvedViewerCount: number,
): number {
  return profile.maxBitrate * approvedViewerCount;
}
