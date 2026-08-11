import {
  bitrateChartCeiling,
  bitratePeak,
  type BitrateSample,
} from '@/lib/metrics/bitrate-history';
import type { CandidatePath } from '@/lib/webrtc/candidate-pair-classifier';
import { formatBitrate, formatPercent } from '@/utils/format';

export const DIAGNOSTICS_HISTORY_WINDOW_MS = 60_000;

export interface PeerDiagnosticsRow {
  id: string;
  name: string;
  path: CandidatePath | 'unknown';
  rttMs: number | null;
  lossRatio: number | null;
  codec: string | null;
  note?: string | null;
}

interface DiagnosticsPanelProps {
  incomingBitrate: number | null;
  outgoingBitrate: number | null;
  history: readonly BitrateSample[];
  peers: readonly PeerDiagnosticsRow[];
  onExport: () => void;
}

export function DiagnosticsPanel({
  incomingBitrate,
  outgoingBitrate,
  history,
  peers,
  onExport,
}: DiagnosticsPanelProps) {
  return (
    <div className="room-diag">
      <div className="room-diag__tiles">
        <div className="room-diag__tile">
          <span>Incoming</span>
          <strong>{formatBitrate(incomingBitrate ?? undefined)}</strong>
        </div>
        <div className="room-diag__tile">
          <span>Outgoing</span>
          <strong>{formatBitrate(outgoingBitrate ?? undefined)}</strong>
        </div>
      </div>

      <BitrateSparkline samples={history} />

      <div className="room-diag__heading">Per peer</div>
      {peers.length > 0 ? (
        <div className="room-diag__peers">
          {peers.map((peer) => (
            <div className="room-diag__peer" key={peer.id}>
              <div className="room-diag__peer-head">
                <strong>{peer.name}</strong>
                <span className={`room-diag__badge room-diag__badge--${peer.path}`}>
                  {peer.path === 'direct' || peer.path === 'relay' ? peer.path : 'p2p?'}
                </span>
              </div>
              <div className="room-diag__stats">
                <span><span>rtt</span><span>{peer.rttMs === null ? '—' : `${peer.rttMs.toFixed(0)} ms`}</span></span>
                <span><span>loss</span><span>{peer.lossRatio === null ? '—' : formatPercent(peer.lossRatio)}</span></span>
                <span><span>codec</span><span>{peer.codec ?? '—'}</span></span>
              </div>
              {peer.note ? <div className="room-diag__note">{peer.note}</div> : null}
            </div>
          ))}
        </div>
      ) : (
        <p className="room-diag__empty">Peer connections appear here once media flows.</p>
      )}

      <button type="button" className="room-diag__export" onClick={onExport}>
        Export redacted report
      </button>
    </div>
  );
}

const SPARK_WIDTH = 280;
const SPARK_HEIGHT = 80;
const SPARK_TOP = 14;

function BitrateSparkline({ samples }: { samples: readonly BitrateSample[] }) {
  const latest = samples.at(-1)?.sampledAt ?? Date.now();
  const peak = bitratePeak(samples);
  const ceiling = bitrateChartCeiling(peak);
  const points = samples.map((sample) => {
    const age = latest - sample.sampledAt;
    const x = Math.max(
      0,
      Math.min(SPARK_WIDTH, SPARK_WIDTH * (1 - age / DIAGNOSTICS_HISTORY_WINDOW_MS)),
    );
    const y = SPARK_HEIGHT - (sample.bitrate / ceiling) * (SPARK_HEIGHT - SPARK_TOP);
    return `${x.toFixed(1)},${y.toFixed(1)}`;
  });
  const first = points[0] ?? `0,${SPARK_HEIGHT}`;
  const last = points.at(-1) ?? first;
  const area = [
    `${first.split(',')[0]},${SPARK_HEIGHT}`,
    ...points,
    `${last.split(',')[0]},${SPARK_HEIGHT}`,
  ].join(' ');

  return (
    <div className="room-diag__spark">
      <svg
        viewBox={`0 0 ${SPARK_WIDTH} ${SPARK_HEIGHT}`}
        preserveAspectRatio="none"
        role="img"
        aria-label={`Bitrate over the last 60 seconds, peaking at ${formatBitrate(peak)}.`}
      >
        <polygon className="room-diag__spark-area" points={area} />
        <polyline className="room-diag__spark-line" points={points.join(' ')} fill="none" />
      </svg>
      <span aria-hidden="true">bitrate · 60s</span>
    </div>
  );
}
