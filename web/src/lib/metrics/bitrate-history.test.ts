import {
  appendBitrateSample,
  bitrateChartCeiling,
  bitratePeak,
  sanitizeBitrate,
} from './bitrate-history';

describe('bitrate history', () => {
  it('keeps only samples from the trailing window', () => {
    const history = [
      { bitrate: 1_000_000, sampledAt: 69_999 },
      { bitrate: 2_000_000, sampledAt: 70_000 },
      { bitrate: 3_000_000, sampledAt: 85_000 },
    ];

    expect(appendBitrateSample(history, { bitrate: 4_000_000, sampledAt: 100_000 })).toEqual([
      { bitrate: 2_000_000, sampledAt: 70_000 },
      { bitrate: 3_000_000, sampledAt: 85_000 },
      { bitrate: 4_000_000, sampledAt: 100_000 },
    ]);
  });

  it('uses a readable ceiling and reports the observed peak', () => {
    const samples = [
      { bitrate: 8_000_000, sampledAt: 1 },
      { bitrate: 18_200_000, sampledAt: 2 },
      { bitrate: 12_000_000, sampledAt: 3 },
    ];

    expect(bitratePeak(samples)).toBe(18_200_000);
    expect(bitrateChartCeiling(18_200_000)).toBe(20_000_000);
    expect(bitrateChartCeiling(0)).toBe(1_000_000);
  });

  it('normalizes unusable readings to zero', () => {
    expect(sanitizeBitrate(-100)).toBe(0);
    expect(sanitizeBitrate(Number.NaN)).toBe(0);
  });
});
