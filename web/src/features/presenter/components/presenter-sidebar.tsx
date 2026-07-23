import type { FormEvent } from 'react';

import { Button } from '@/components/ui/button';
import type { RoomSnapshot } from '@/generated/protocol';
import type { BitrateSample } from '../metrics/bitrate-history';
import type { PresenterPeerStatus } from '../webrtc/presenter-connection-manager';
import { formatBitrate, formatPercent, formatResolution } from '@/utils/format';
import { BitrateHistoryGraph } from './bitrate-history-graph';

interface PresenterSidebarProps {
  snapshot: RoomSnapshot | null;
  viewerUrl: string;
  copyState: 'idle' | 'copied' | 'failed';
  capacityDraft: string;
  capacityValid: boolean;
  maximumViewers: number;
  aggregateBitrate: number;
  peakBitrate: number;
  bitrateHistory: readonly BitrateSample[];
  statuses: readonly PresenterPeerStatus[];
  directCount: number;
  relayCount: number;
  expiresIn: string;
  live: boolean;
  ended: boolean;
  onCopyInvite: () => void;
  onCapacityDraftChange: (value: string) => void;
  onUpdateCapacity: () => void;
  onApproveViewer: (peerId: string) => void;
  onRejectViewer: (peerId: string) => void;
  onRestartViewerIce: (peerId: string) => void;
  onRemoveViewer: (peerId: string) => void;
}

export function PresenterSidebar({
  snapshot,
  viewerUrl,
  copyState,
  capacityDraft,
  capacityValid,
  maximumViewers,
  aggregateBitrate,
  peakBitrate,
  bitrateHistory,
  statuses,
  directCount,
  relayCount,
  expiresIn,
  live,
  ended,
  onCopyInvite,
  onCapacityDraftChange,
  onUpdateCapacity,
  onApproveViewer,
  onRejectViewer,
  onRestartViewerIce,
  onRemoveViewer,
}: PresenterSidebarProps) {
  const approvedViewers = snapshot?.approvedViewers ?? [];
  const pendingViewers = snapshot?.pendingViewers ?? [];
  const requiresApproval = snapshot?.accessPolicy === 'approvalRequired';
  const connectionDescription = describeConnections(statuses.length, directCount, relayCount);

  const submitCapacity = (event: FormEvent<HTMLFormElement>) => {
    event.preventDefault();
    onUpdateCapacity();
  };

  return (
    <aside className="presenter-sidebar" aria-label="Room and connection controls">
      <section className="presenter-sidebar__section presenter-sidebar__invite" aria-labelledby="invite-viewers-title">
        <div className="presenter-sidebar__heading">
          <div>
            <h2 id="invite-viewers-title">Invite viewers</h2>
            <span>{requiresApproval ? 'Approval required' : 'Public link'} · Expires in {expiresIn}</span>
          </div>
        </div>

        <div className="presenter-sidebar__invite-row">
          <code title={viewerUrl}>{safeInviteLabel(viewerUrl)}</code>
          <Button variant="primary" onClick={onCopyInvite} disabled={!viewerUrl || ended}>
            {copyState === 'copied' ? 'Copied' : copyState === 'failed' ? 'Copy failed' : 'Copy link'}
          </Button>
        </div>

        <div className="presenter-sidebar__capacity-row">
          <form onSubmit={submitCapacity}>
            <label htmlFor="presenter-room-limit">Max viewers</label>
            <input
              id="presenter-room-limit"
              aria-label="Room limit"
              type="number"
              min="1"
              max="10"
              step="1"
              inputMode="numeric"
              value={capacityDraft}
              onChange={(event) => onCapacityDraftChange(event.target.value)}
              disabled={!snapshot || ended}
              required
            />
            <Button
              type="submit"
              variant="quiet"
              disabled={!snapshot || ended || !capacityValid || Number(capacityDraft) === maximumViewers}
            >
              Apply
            </Button>
          </form>
          <span>{approvedViewers.length} of {maximumViewers} slots used</span>
        </div>
      </section>

      <section className="presenter-sidebar__section presenter-sidebar__health" aria-labelledby="connection-health-title">
        <div className="presenter-sidebar__heading presenter-sidebar__heading--health">
          <div>
            <h2 id="connection-health-title">Connection health</h2>
            <span>{connectionDescription}</span>
          </div>
          <span className="presenter-sidebar__live-window">
            {live ? 'Live · 30 sec' : '30 sec window'}
          </span>
        </div>

        <p className="presenter-sidebar__bitrate">
          <strong>{formatBitrate(aggregateBitrate)}</strong>
          <span>upload</span>
        </p>
        <BitrateHistoryGraph currentBitrate={aggregateBitrate} samples={bitrateHistory} />
        <div className="presenter-sidebar__health-meta">
          <span>{statuses.length} active {statuses.length === 1 ? 'sender' : 'senders'}</span>
          <span>Peak {formatBitrate(peakBitrate)}</span>
        </div>
      </section>

      {requiresApproval && pendingViewers.length > 0 ? (
        <section className="presenter-sidebar__section presenter-sidebar__requests" aria-labelledby="viewer-requests-title">
          <div className="presenter-sidebar__heading">
            <h2 id="viewer-requests-title">Waiting for approval</h2>
            <span>{pendingViewers.length} waiting</span>
          </div>
          <ul className="presenter-sidebar__request-list pending-list">
            {pendingViewers.map((viewer) => (
              <li key={viewer.peerId}>
                <ViewerIdentity displayName={viewer.displayName} peerId={viewer.peerId} fallback="Unnamed viewer" />
                <div className="presenter-sidebar__request-actions">
                  <Button
                    variant="primary"
                    onClick={() => onApproveViewer(viewer.peerId)}
                    disabled={approvedViewers.length >= maximumViewers}
                  >
                    Approve
                  </Button>
                  <Button variant="quiet" onClick={() => onRejectViewer(viewer.peerId)}>Reject</Button>
                </div>
              </li>
            ))}
          </ul>
        </section>
      ) : null}

      <section className="presenter-sidebar__section presenter-sidebar__watching" aria-labelledby="watching-title">
        <div className="presenter-sidebar__heading">
          <h2 id="watching-title">Who&apos;s watching</h2>
          <span>{approvedViewers.length} active</span>
        </div>

        {approvedViewers.length > 0 ? (
          <ul className="presenter-sidebar__watcher-list">
            {approvedViewers.map((viewer) => {
              const status = statuses.find((candidate) => candidate.peerId === viewer.peerId);
              const health = viewerHealth(status);
              return (
                <li className="presenter-console__watcher peer-card" key={viewer.peerId}>
                  <div className="presenter-console__watcher-summary">
                    <ViewerIdentity
                      displayName={viewer.displayName}
                      peerId={viewer.peerId}
                      fallback={requiresApproval ? 'Unnamed viewer' : 'Anonymous viewer'}
                    />
                    <span className={`presenter-console__viewer-health presenter-console__viewer-health--${health.tone}`}>
                      <i aria-hidden="true" /> {health.label}
                    </span>
                    <button
                      type="button"
                      className="presenter-console__remove-viewer"
                      aria-label="Remove viewer"
                      title="Remove viewer"
                      onClick={() => onRemoveViewer(viewer.peerId)}
                    >
                      ×
                    </button>
                  </div>

                  <details className="presenter-console__viewer-details">
                    <summary>Connection details</summary>
                    <dl className="metric-grid">
                      <Metric label="Path" value={pathLabel(status?.metrics?.candidatePath)} />
                      <Metric label="Bitrate" value={formatBitrate(status?.metrics?.bitrate)} />
                      <Metric label="Encoded" value={formatResolution(status?.metrics?.frameWidth, status?.metrics?.frameHeight)} />
                      <Metric label="Frame rate" value={status?.metrics?.framesPerSecond ? `${status.metrics.framesPerSecond.toFixed(0)} FPS` : 'Unavailable'} />
                      <Metric label="Round trip" value={status?.metrics?.roundTripTimeMs ? `${status.metrics.roundTripTimeMs.toFixed(0)} ms` : 'Unavailable'} />
                      <Metric label="Packet loss" value={formatPercent(status?.metrics?.packetLossRatio)} />
                      <Metric label="Codec" value={status?.metrics?.codec ?? 'Negotiating'} />
                      <Metric label="Profile" value={status?.profile.label ?? 'Waiting'} />
                    </dl>
                    <p>{status?.lastAdaptationReason ?? 'Quality control starts with the media connection.'}</p>
                    <Button variant="quiet" onClick={() => onRestartViewerIce(viewer.peerId)} disabled={!status}>
                      Restart ICE
                    </Button>
                  </details>
                </li>
              );
            })}
          </ul>
        ) : (
          <p className="presenter-sidebar__empty">
            {requiresApproval
              ? 'Approved viewers appear here when they connect.'
              : 'Viewers appear here when they open the public link.'}
          </p>
        )}
      </section>
    </aside>
  );
}

function ViewerIdentity({
  displayName,
  peerId,
  fallback,
}: {
  displayName: string | null;
  peerId: string;
  fallback: string;
}) {
  const label = displayName || fallback;
  return (
    <div className="presenter-console__viewer-identity viewer-identity">
      <span className="avatar" aria-hidden="true">{initials(displayName)}</span>
      <span>
        <strong>{label}</strong>
        <code>{peerId.slice(0, 10)}</code>
      </span>
    </div>
  );
}

function Metric({ label, value }: { label: string; value: string }) {
  return <div><dt>{label}</dt><dd>{value}</dd></div>;
}

function viewerHealth(status: PresenterPeerStatus | undefined): {
  label: 'Good' | 'Limited' | 'Connecting' | 'Failed';
  tone: 'good' | 'limited' | 'connecting' | 'failed';
} {
  if (status?.connectionState === 'failed' || status?.connectionState === 'disconnected') {
    return { label: 'Failed', tone: 'failed' };
  }
  if (
    status?.metrics?.qualityLimitationReason === 'bandwidth' ||
    status?.metrics?.qualityLimitationReason === 'cpu' ||
    (status?.metrics?.packetLossRatio ?? 0) > 0.05 ||
    (status?.metrics?.roundTripTimeMs ?? 0) > 500
  ) {
    return { label: 'Limited', tone: 'limited' };
  }
  if (status?.connectionState === 'connected') return { label: 'Good', tone: 'good' };
  return { label: 'Connecting', tone: 'connecting' };
}

function describeConnections(statusCount: number, directCount: number, relayCount: number): string {
  if (relayCount > 0) {
    return `${relayCount} relayed · ${directCount} direct`;
  }
  if (statusCount > 0 && directCount > 0) return 'Direct peer-to-peer, no server relay';
  return 'Waiting for an active peer connection';
}

function safeInviteLabel(viewerUrl: string): string {
  if (!viewerUrl) return 'Invite becomes available after room creation';
  try {
    const url = new URL(viewerUrl);
    return `${url.origin}${url.pathname}${url.search}#••••••••••••`;
  } catch {
    return 'Secure viewer invite';
  }
}

function initials(name: string | null): string {
  if (!name) return '?';
  return name.split(/\s+/u).slice(0, 2).map((part) => part[0]?.toUpperCase() ?? '').join('');
}

function pathLabel(path: string | undefined): string {
  if (path === 'relay') return 'TURN relay';
  if (path === 'direct') return 'Direct';
  if (path === 'determining') return 'Determining';
  return 'Unavailable';
}
