export type CandidatePath = 'direct' | 'relay' | 'determining' | 'unavailable';

export interface CandidateSummary {
  path: CandidatePath;
  localType?: string;
  remoteType?: string;
}

export interface BrowserRtcReport {
  id: string;
  type: string;
  [key: string]: unknown;
}

export function classifyCandidatePair(reports: readonly BrowserRtcReport[]): CandidateSummary {
  const byId = new Map(reports.map((report) => [report.id, report]));
  const transport = reports.find((report) => report.type === 'transport');
  const selectedId = stringValue(transport?.selectedCandidatePairId);
  const pair =
    (selectedId ? byId.get(selectedId) : undefined) ??
    reports.find(
      (report) =>
        report.type === 'candidate-pair' &&
        (report.selected === true || (report.nominated === true && report.state === 'succeeded')),
    );
  if (!pair) {
    return reports.some((report) => report.type === 'candidate-pair')
      ? { path: 'determining' }
      : { path: 'unavailable' };
  }
  const local = byId.get(stringValue(pair.localCandidateId) ?? '');
  const remote = byId.get(stringValue(pair.remoteCandidateId) ?? '');
  const localType = stringValue(local?.candidateType);
  const remoteType = stringValue(remote?.candidateType);
  return {
    path: localType === 'relay' || remoteType === 'relay' ? 'relay' : 'direct',
    localType,
    remoteType,
  };
}

function stringValue(value: unknown): string | undefined {
  return typeof value === 'string' ? value : undefined;
}
