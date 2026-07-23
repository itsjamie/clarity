import { calculateBitrate } from './stats-collector';

describe('WebRTC counter deltas', () => {
  it('calculates bitrate from byte and timestamp deltas', () => {
    expect(calculateBitrate({ bytes: 1_000, timestamp: 1_000 }, 3_000, 3_000)).toBe(8_000);
  });

  it('tolerates missing values, counter resets, and zero intervals', () => {
    expect(calculateBitrate(null, 3_000, 3_000)).toBeUndefined();
    expect(calculateBitrate({ bytes: 3_000, timestamp: 1_000 }, 1_000, 3_000)).toBeUndefined();
    expect(calculateBitrate({ bytes: 1_000, timestamp: 3_000 }, 2_000, 3_000)).toBeUndefined();
  });
});
