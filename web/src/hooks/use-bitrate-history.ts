import { useEffect, useRef, useState } from 'react';

import {
  appendBitrateSample,
  BITRATE_SAMPLE_INTERVAL_MS,
  sanitizeBitrate,
  type BitrateSample,
} from '@/lib/metrics/bitrate-history';

export function useBitrateHistory(
  bitrate: number,
  historyKey: string,
  windowMs?: number,
): readonly BitrateSample[] {
  const currentBitrate = useRef(sanitizeBitrate(bitrate));
  const [samples, setSamples] = useState<BitrateSample[]>(() => [sampleNow(currentBitrate.current)]);

  useEffect(() => {
    currentBitrate.current = sanitizeBitrate(bitrate);
  }, [bitrate]);

  useEffect(() => {
    setSamples([sampleNow(currentBitrate.current)]);
    const timer = window.setInterval(() => {
      setSamples((history) => appendBitrateSample(history, sampleNow(currentBitrate.current), windowMs));
    }, BITRATE_SAMPLE_INTERVAL_MS);

    return () => window.clearInterval(timer);
  }, [historyKey, windowMs]);

  return samples;
}

function sampleNow(bitrate: number): BitrateSample {
  return { bitrate, sampledAt: Date.now() };
}
