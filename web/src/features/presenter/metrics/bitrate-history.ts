export const BITRATE_HISTORY_WINDOW_MS = 30_000;
export const BITRATE_SAMPLE_INTERVAL_MS = 1_000;

export interface BitrateSample {
  bitrate: number;
  sampledAt: number;
}

export function appendBitrateSample(
  history: readonly BitrateSample[],
  sample: BitrateSample,
  windowMs = BITRATE_HISTORY_WINDOW_MS,
): BitrateSample[] {
  const cutoff = sample.sampledAt - windowMs;
  return [...history.filter((entry) => entry.sampledAt >= cutoff), sample];
}

export function sanitizeBitrate(bitrate: number): number {
  return Number.isFinite(bitrate) ? Math.max(0, bitrate) : 0;
}

export function bitratePeak(samples: readonly BitrateSample[]): number {
  return samples.reduce((peak, sample) => Math.max(peak, sample.bitrate), 0);
}

export function bitrateChartCeiling(peak: number): number {
  const minimumCeiling = 1_000_000;
  if (!Number.isFinite(peak) || peak <= minimumCeiling) return minimumCeiling;

  const magnitude = 10 ** Math.floor(Math.log10(peak));
  const normalized = peak / magnitude;
  const rounded = normalized <= 1 ? 1 : normalized <= 2 ? 2 : normalized <= 5 ? 5 : 10;
  return rounded * magnitude;
}
