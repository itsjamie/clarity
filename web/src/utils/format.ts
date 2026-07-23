export function formatBitrate(bitsPerSecond: number | undefined): string {
  if (bitsPerSecond === undefined) return 'Unavailable';
  if (bitsPerSecond >= 1_000_000) return `${(bitsPerSecond / 1_000_000).toFixed(1)} Mbps`;
  if (bitsPerSecond >= 1_000) return `${(bitsPerSecond / 1_000).toFixed(0)} Kbps`;
  return `${bitsPerSecond.toFixed(0)} bps`;
}

export function formatResolution(width?: number, height?: number): string {
  return width && height ? `${width} × ${height}` : 'Unavailable';
}

export function formatPercent(ratio: number | undefined): string {
  return ratio === undefined ? 'Unavailable' : `${(ratio * 100).toFixed(1)}%`;
}

export function formatRemaining(isoTimestamp: string, now = Date.now()): string {
  const milliseconds = Math.max(0, Date.parse(isoTimestamp) - now);
  const hours = Math.floor(milliseconds / 3_600_000);
  const minutes = Math.floor((milliseconds % 3_600_000) / 60_000);
  return hours > 0 ? `${hours}h ${minutes}m` : `${minutes}m`;
}
