import {
  classifyCandidatePair,
  type BrowserRtcReport,
  type CandidatePath,
} from './candidate-pair-classifier';

export interface WebRtcMetrics {
  bitrate?: number;
  frameWidth?: number;
  frameHeight?: number;
  framesPerSecond?: number;
  frames?: number;
  framesDropped?: number;
  packets?: number;
  packetsLost?: number;
  packetLossRatio?: number;
  roundTripTimeMs?: number;
  jitterMs?: number;
  availableOutgoingBitrate?: number;
  codec?: string;
  candidatePath: CandidatePath;
  localCandidateType?: string;
  remoteCandidateType?: string;
  qualityLimitationReason?: 'none' | 'cpu' | 'bandwidth' | 'other';
  sampledAt: number;
}

interface CounterState {
  bytes: number;
  timestamp: number;
}

export class WebRtcStatsCollector {
  readonly #connection: RTCPeerConnection;
  readonly #direction: 'outbound' | 'inbound';
  readonly #onMetrics: (metrics: WebRtcMetrics) => void;
  #timer: number | null = null;
  #previous: CounterState | null = null;

  public constructor(
    connection: RTCPeerConnection,
    direction: 'outbound' | 'inbound',
    onMetrics: (metrics: WebRtcMetrics) => void,
  ) {
    this.#connection = connection;
    this.#direction = direction;
    this.#onMetrics = onMetrics;
  }

  public start(intervalMs = 2_000): void {
    if (this.#timer !== null) return;
    void this.sample();
    this.#timer = window.setInterval(() => void this.sample(), intervalMs);
  }

  public stop(): void {
    if (this.#timer !== null) window.clearInterval(this.#timer);
    this.#timer = null;
    this.#previous = null;
  }

  public async sample(): Promise<WebRtcMetrics | null> {
    if (this.#connection.connectionState === 'closed') return null;
    const stats = await this.#connection.getStats();
    const reports: BrowserRtcReport[] = [];
    stats.forEach((report: unknown) => {
      if (isReport(report)) reports.push(report);
    });
    const primaryType = this.#direction === 'outbound' ? 'outbound-rtp' : 'inbound-rtp';
    const primary = reports.find(
      (report) => report.type === primaryType && report.kind === 'video' && report.isRemote !== true,
    );
    if (!primary) return null;
    const sampledAt = numberValue(primary.timestamp) ?? performance.now();
    const bytes = numberValue(
      this.#direction === 'outbound' ? primary.bytesSent : primary.bytesReceived,
    );
    const bitrate = calculateBitrate(this.#previous, bytes, sampledAt);
    if (bytes !== undefined) this.#previous = { bytes, timestamp: sampledAt };
    const remoteInbound = reports.find(
      (report) => report.type === 'remote-inbound-rtp' && report.kind === 'video',
    );
    const packets = numberValue(
      this.#direction === 'outbound' ? primary.packetsSent : primary.packetsReceived,
    );
    const packetsLost = numberValue(
      this.#direction === 'outbound' ? remoteInbound?.packetsLost : primary.packetsLost,
    );
    const candidate = classifyCandidatePair(reports);
    const codecId = stringValue(primary.codecId);
    const codec = codecId ? reports.find((report) => report.id === codecId) : undefined;
    const metrics: WebRtcMetrics = {
      bitrate,
      frameWidth: numberValue(primary.frameWidth),
      frameHeight: numberValue(primary.frameHeight),
      framesPerSecond: numberValue(primary.framesPerSecond),
      frames: numberValue(
        this.#direction === 'outbound' ? primary.framesEncoded : primary.framesDecoded,
      ),
      framesDropped: numberValue(
        this.#direction === 'outbound' ? primary.framesDroppedByEncoder : primary.framesDropped,
      ),
      packets,
      packetsLost,
      packetLossRatio:
        packets !== undefined && packetsLost !== undefined && packets + packetsLost > 0
          ? Math.max(0, packetsLost) / (packets + Math.max(0, packetsLost))
          : undefined,
      roundTripTimeMs: secondsToMilliseconds(
        numberValue(remoteInbound?.roundTripTime ?? candidatePairValue(reports, 'currentRoundTripTime')),
      ),
      jitterMs: secondsToMilliseconds(
        numberValue(this.#direction === 'outbound' ? remoteInbound?.jitter : primary.jitter),
      ),
      availableOutgoingBitrate: numberValue(
        candidatePairValue(reports, 'availableOutgoingBitrate'),
      ),
      codec: stringValue(codec?.mimeType),
      candidatePath: candidate.path,
      localCandidateType: candidate.localType,
      remoteCandidateType: candidate.remoteType,
      qualityLimitationReason: limitationValue(primary.qualityLimitationReason),
      sampledAt,
    };
    this.#onMetrics(metrics);
    return metrics;
  }
}

export function calculateBitrate(
  previous: CounterState | null,
  currentBytes: number | undefined,
  currentTimestamp: number,
): number | undefined {
  if (!previous || currentBytes === undefined || currentBytes < previous.bytes) return undefined;
  const elapsedMilliseconds = currentTimestamp - previous.timestamp;
  if (elapsedMilliseconds <= 0) return undefined;
  return ((currentBytes - previous.bytes) * 8 * 1_000) / elapsedMilliseconds;
}

function candidatePairValue(reports: readonly BrowserRtcReport[], key: string): unknown {
  const transport = reports.find((report) => report.type === 'transport');
  const selectedId = stringValue(transport?.selectedCandidatePairId);
  const pair = reports.find(
    (report) =>
      report.type === 'candidate-pair' &&
      (report.id === selectedId || report.selected === true || (report.nominated === true && report.state === 'succeeded')),
  );
  return pair?.[key];
}

function isReport(value: unknown): value is BrowserRtcReport {
  if (typeof value !== 'object' || value === null) return false;
  const candidate = value as Record<string, unknown>;
  return typeof candidate.id === 'string' && typeof candidate.type === 'string';
}

function numberValue(value: unknown): number | undefined {
  return typeof value === 'number' && Number.isFinite(value) ? value : undefined;
}

function stringValue(value: unknown): string | undefined {
  return typeof value === 'string' ? value : undefined;
}

function secondsToMilliseconds(value: number | undefined): number | undefined {
  return value === undefined ? undefined : value * 1_000;
}

function limitationValue(value: unknown): WebRtcMetrics['qualityLimitationReason'] {
  return value === 'none' || value === 'cpu' || value === 'bandwidth' || value === 'other'
    ? value
    : undefined;
}
