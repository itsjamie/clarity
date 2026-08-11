/** Builds the header's mono summary: "CODE · direct · 5 here · 3h 12m left". */
export function roomMetaLine(
  code: string,
  path: string,
  peerCount: number,
  expiresIn: string | null,
): string {
  const parts = [code, path, `${peerCount} here`];
  if (expiresIn) parts.push(`${expiresIn} left`);
  return parts.join(' · ');
}
