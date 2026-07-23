import { useEffect, useMemo, useState } from 'react';

import { Button } from '@/components/ui/button';
import { PROTOCOL_VERSION } from '@/config/environment';
import {
  PUBLIC_ROOM_VIEWERS,
  isValidViewerLimit,
} from '@/config/room';
import { useNow } from '@/hooks/use-now';
import { useSessionState } from '@/hooks/use-session-state';
import { CodecCapabilityService } from '@/lib/webrtc/codec-capability-service';
import { initialProfile } from '@/lib/webrtc/profiles';
import { formatRemaining } from '@/utils/format';
import { useBitrateHistory } from '../hooks/use-bitrate-history';
import { bitratePeak } from '../metrics/bitrate-history';
import { PresenterSession } from '../session/presenter-session';
import { PresenterSidebar } from './presenter-sidebar';
import { PresenterStage } from './presenter-stage';
import '../styles/presenter-console.css';

interface PresenterWorkspaceProps {
  roomId: string;
  presenterSecret: string;
  viewerUrl: string;
}

export function PresenterWorkspace(props: PresenterWorkspaceProps) {
  const session = useMemo(
    () => new PresenterSession(props.roomId, props.presenterSecret, props.viewerUrl),
    [props.presenterSecret, props.roomId, props.viewerUrl],
  );
  const state = useSessionState(session);
  const now = useNow();
  const [copyState, setCopyState] = useState<'idle' | 'copied' | 'failed'>('idle');
  const [confirmEnd, setConfirmEnd] = useState(false);
  const [capacityDraft, setCapacityDraft] = useState('');
  const codecModes = useMemo(() => new CodecCapabilityService().supportedModes(), []);

  useEffect(() => {
    session.connect();
    return () => session.disconnect();
  }, [session]);

  useEffect(() => {
    if (state.snapshot) setCapacityDraft(String(state.snapshot.maximumViewers));
  }, [state.snapshot]);

  const statuses = Object.values(state.peerStatuses);
  const aggregateBitrate = statuses.reduce(
    (total, status) => total + (status.metrics?.bitrate ?? 0),
    0,
  );
  const bitrateHistory = useBitrateHistory(aggregateBitrate, props.roomId);
  const maximumViewers = state.snapshot?.maximumViewers ?? PUBLIC_ROOM_VIEWERS;
  const parsedCapacity = Number(capacityDraft);
  const requestedProfile = initialProfile(state.captureMode);
  const directCount = statuses.filter((status) => status.metrics?.candidatePath === 'direct').length;
  const relayCount = statuses.filter((status) => status.metrics?.candidatePath === 'relay').length;

  const copyInvite = async () => {
    try {
      await navigator.clipboard.writeText(state.viewerUrl);
      setCopyState('copied');
      window.setTimeout(() => setCopyState('idle'), 2_000);
    } catch {
      setCopyState('failed');
    }
  };

  const downloadDiagnostics = () => {
    const blob = new Blob([session.diagnosticsJson()], { type: 'application/json' });
    const url = URL.createObjectURL(blob);
    const anchor = document.createElement('a');
    anchor.href = url;
    anchor.download = `clarity-diagnostics-${props.roomId}.json`;
    anchor.click();
    URL.revokeObjectURL(url);
  };

  return (
    <div className="app-shell presenter-console-shell">
      <main className="presenter-console">
        <h1 className="sr-only">Presenter console</h1>
        <div className="presenter-console__grid">
          <PresenterStage
            state={state}
            requestedProfile={requestedProfile}
            codecModes={codecModes}
            confirmEnd={confirmEnd}
            onSetPreferences={(preferences) => void session.setPreferences(preferences)}
            onStartSharing={() => void session.startSharing()}
            onChangeSource={() => void session.changeSource()}
            onRequestEnd={() => setConfirmEnd(true)}
            onCancelEnd={() => setConfirmEnd(false)}
            onEndRoom={() => {
              setConfirmEnd(false);
              session.endRoom();
            }}
          />

          <PresenterSidebar
            snapshot={state.snapshot}
            viewerUrl={state.viewerUrl}
            copyState={copyState}
            capacityDraft={capacityDraft}
            capacityValid={isValidViewerLimit(parsedCapacity)}
            maximumViewers={maximumViewers}
            aggregateBitrate={aggregateBitrate}
            peakBitrate={Math.max(aggregateBitrate, bitratePeak(bitrateHistory))}
            bitrateHistory={bitrateHistory}
            statuses={statuses}
            directCount={directCount}
            relayCount={relayCount}
            expiresIn={state.snapshot ? formatRemaining(state.snapshot.expiresAt, now) : 'Loading'}
            live={state.captureActive}
            ended={state.ended}
            onCopyInvite={() => void copyInvite()}
            onCapacityDraftChange={setCapacityDraft}
            onUpdateCapacity={() => {
              if (isValidViewerLimit(parsedCapacity) && parsedCapacity !== maximumViewers) {
                session.updateCapacity(parsedCapacity);
              }
            }}
            onApproveViewer={(peerId) => session.approveViewer(peerId)}
            onRejectViewer={(peerId) => session.rejectViewer(peerId)}
            onRestartViewerIce={(peerId) => void session.restartViewerIce(peerId)}
            onRemoveViewer={(peerId) => session.kickViewer(peerId)}
          />
        </div>

        <details className="presenter-console__diagnostics">
          <summary>Advanced diagnostics</summary>
          <div className="presenter-console__diagnostics-content">
            <dl>
              <Diagnostic label="Room" value={props.roomId.slice(0, 8)} />
              <Diagnostic label="Protocol" value={String(PROTOCOL_VERSION)} />
              <Diagnostic label="WebSocket" value={state.signaling} />
              <Diagnostic label="Capture hint" value={state.captureSettings?.contentHint ?? 'Unavailable'} />
              <Diagnostic label="Active peer tasks" value={String(statuses.length)} />
            </dl>
            <Button variant="secondary" onClick={downloadDiagnostics}>Export sanitized JSON</Button>
          </div>
        </details>
      </main>

      <div className="sr-only" aria-live="polite">
        {state.snapshot?.accessPolicy === 'approvalRequired'
          ? `${state.snapshot.pendingViewers.length} viewers waiting.`
          : 'Public link admission is active.'}{' '}
        {state.snapshot?.approvedViewers.length ?? 0} viewers active. Signaling {state.signaling}.
      </div>
    </div>
  );
}

function Diagnostic({ label, value }: { label: string; value: string }) {
  return <div><dt>{label}</dt><dd>{value}</dd></div>;
}
