import { estimatedUploadBitsPerSecond, MOTION_PROFILES, TEXT_PROFILES } from './profiles';

describe('encoding profiles', () => {
  it('preserves text resolution longer and motion frame rate longer', () => {
    expect(TEXT_PROFILES.slice(0, 3).every((profile) => profile.scaleResolutionDownBy === 1)).toBe(true);
    expect(MOTION_PROFILES.slice(0, 2).every((profile) => profile.maxFramerate === 60)).toBe(true);
  });

  it('calculates aggregate estimated upload across viewers', () => {
    expect(estimatedUploadBitsPerSecond(TEXT_PROFILES[0]!, 4)).toBe(72_000_000);
  });
});
