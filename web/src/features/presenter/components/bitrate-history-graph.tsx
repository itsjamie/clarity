import { formatBitrate } from '@/utils/format';
import {
  BITRATE_HISTORY_WINDOW_MS,
  bitrateChartCeiling,
  bitratePeak,
  type BitrateSample,
} from '@/lib/metrics/bitrate-history';

interface BitrateHistoryGraphProps {
  currentBitrate: number;
  samples: readonly BitrateSample[];
}

const CHART_WIDTH = 320;
const CHART_LEFT = 6;
const CHART_RIGHT = CHART_WIDTH - 6;
const CHART_TOP = 10;
const CHART_BOTTOM = 92;

export function BitrateHistoryGraph({ currentBitrate, samples }: BitrateHistoryGraphProps) {
  const latestTimestamp = samples.at(-1)?.sampledAt ?? Date.now();
  const peak = Math.max(currentBitrate, bitratePeak(samples));
  const ceiling = bitrateChartCeiling(peak);
  const points = samples.map((sample) => ({
    x: Math.max(
      CHART_LEFT,
      Math.min(
        CHART_RIGHT,
        CHART_LEFT + ((sample.sampledAt - (latestTimestamp - BITRATE_HISTORY_WINDOW_MS))
          / BITRATE_HISTORY_WINDOW_MS) * (CHART_RIGHT - CHART_LEFT),
      ),
    ),
    y: CHART_BOTTOM - (sample.bitrate / ceiling) * (CHART_BOTTOM - CHART_TOP),
  }));
  const firstPoint = points[0] ?? { x: CHART_RIGHT, y: CHART_BOTTOM };
  const lastPoint = points.at(-1) ?? firstPoint;
  const linePoints = points.map((point) => `${point.x.toFixed(2)},${point.y.toFixed(2)}`).join(' ');
  const areaPoints = [
    `${firstPoint.x.toFixed(2)},${CHART_BOTTOM}`,
    linePoints,
    `${lastPoint.x.toFixed(2)},${CHART_BOTTOM}`,
  ].join(' ');
  const description = `Upload bitrate over the last 30 seconds. Current ${formatBitrate(currentBitrate)}; peak ${formatBitrate(peak)}.`;

  return (
    <figure className="bitrate-chart">
      <div className="bitrate-chart__plot">
        <div className="bitrate-chart__scale" aria-hidden="true">
          <span>{formatBitrate(ceiling)}</span>
          <span>0</span>
        </div>
        <svg
          className="bitrate-chart__svg"
          viewBox={`0 0 ${CHART_WIDTH} 102`}
          preserveAspectRatio="none"
          role="img"
          aria-label={description}
        >
          <g className="bitrate-chart__grid" aria-hidden="true">
            <line x1="0" y1={CHART_TOP} x2={CHART_WIDTH} y2={CHART_TOP} />
            <line x1="0" y1={(CHART_TOP + CHART_BOTTOM) / 2} x2={CHART_WIDTH} y2={(CHART_TOP + CHART_BOTTOM) / 2} />
            <line x1="0" y1={CHART_BOTTOM} x2={CHART_WIDTH} y2={CHART_BOTTOM} />
            {[0, 80, 160, 240, 320].map((x) => (
              <line key={x} x1={x} y1={CHART_TOP} x2={x} y2={CHART_BOTTOM} />
            ))}
          </g>
          <polygon className="bitrate-chart__area" points={areaPoints} aria-hidden="true" />
          <polyline className="bitrate-chart__line" points={linePoints} aria-hidden="true" />
          <circle className="bitrate-chart__halo" cx={lastPoint.x} cy={lastPoint.y} r="7" aria-hidden="true" />
          <circle className="bitrate-chart__point" cx={lastPoint.x} cy={lastPoint.y} r="3" aria-hidden="true" />
        </svg>
      </div>
      <figcaption>
        <span>30s ago</span>
        <span>Now</span>
      </figcaption>
    </figure>
  );
}
