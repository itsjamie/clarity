import { useEffect, useMemo, useState } from 'react';
import { useNavigate } from 'react-router-dom';

import { Button } from '@/components/ui/button';
import {
  PUBLIC_ROOM_VIEWERS,
  isValidViewerLimit,
} from '@/config/room';
import { ChatPanel } from '@/components/room/chat-panel';
import {
  DIAGNOSTICS_HISTORY_WINDOW_MS,
  DiagnosticsPanel,
  type PeerDiagnosticsRow,
} from '@/components/room/diagnostics-panel';
import { RoomHeader } from '@/components/room/room-header';
import { roomMetaLine } from '@/components/room/room-meta';
import { RoomPanel } from '@/components/room/room-panel';
import { TheatreChat } from '@/components/room/theatre-chat';
import { useTheatreMode } from '@/hooks/use-theatre-mode';
import { useNow } from '@/hooks/use-now';
import { useSessionState } from '@/hooks/use-session-state';
import {
  announceHosting,
  ensurePresenceStarted,
  identityStore,
} from '@/lib/presence/presence-service';
import { loadRoomCaptureMode } from '@/lib/storage/session-storage';
import { CodecCapabilityService } from '@/lib/webrtc/codec-capability-service';
import { initialProfile } from '@/lib/webrtc/profiles';
import { formatRemaining } from '@/utils/format';
import { useBitrateHistory } from '@/hooks/use-bitrate-history';
import { BITRATE_HISTORY_WINDOW_MS, bitratePeak } from '@/lib/metrics/bitrate-history';
import { PresenterSession } from '../session/presenter-session';
import { PresenterSidebar } from './presenter-sidebar';
import { PresenterStage } from './presenter-stage';
import '../styles/presenter-console.css';
import '@/styles/room.css';

type PanelTab = 'chat' | 'diagnostics' | 'room';

interface PresenterWorkspaceProps {
  roomId: string;
  presenterSecret: string;
  viewerUrl: string;
}

export function PresenterWorkspace(props: PresenterWorkspaceProps) {
  const session = useMemo(
    () =>
      new PresenterSession(props.roomId, props.presenterSecret, props.viewerUrl, undefined, {
        captureMode: loadRoomCaptureMode(props.roomId) ?? undefined,
      }),
    [props.presenterSecret, props.roomId, props.viewerUrl],
  );
  const state = useSessionState(session);
  const identityDisplayName = useSessionState(identityStore).displayName;
  const now = useNow();
  const navigate = useNavigate();
  const [copyState, setCopyState] = useState<'idle' | 'copied' | 'failed'>('idle');
  const [confirmClose, setConfirmClose] = useState(false);
  const [capacityDraft, setCapacityDraft] = useState('');
  const [panelTab, setPanelTab] = useState<PanelTab>('chat');
  const { theatre, toggleTheatre } = useTheatreMode();
  const [theatreChatOpen, setTheatreChatOpen] = useState(true);
  const codecModes = useMemo(() => new CodecCapabilityService().supportedModes(), []);

  useEffect(() => {
    session.connect();
    return () => session.disconnect();
  }, [session]);

  useEffect(() => {
    if (identityDisplayName) session.setDisplayName(identityDisplayName);
  }, [identityDisplayName, session]);

  useEffect(() => {
    if (state.snapshot) setCapacityDraft(String(state.snapshot.maximumViewers));
  }, [state.snapshot]);

  const approvedViewers = state.snapshot?.approvedViewers ?? [];
  const pendingViewers = state.snapshot?.pendingViewers ?? [];
  const approvedViewerCount = approvedViewers.length;
  const sharingState = state.snapshot?.sharingState ?? 'idle';
  useEffect(() => {
    ensurePresenceStarted();
    if (!state.snapshot) return;
    if (state.ended) {
      announceHosting(null);
      return;
    }
    announceHosting({
      room: {
        roomId: props.roomId,
        viewerUrl: props.viewerUrl,
        viewerCount: approvedViewerCount,
        sharingState,
      },
      presenterSecret: props.presenterSecret,
    });
  }, [
    approvedViewerCount,
    props.presenterSecret,
    props.roomId,
    props.viewerUrl,
    sharingState,
    state.ended,
    state.snapshot,
  ]);
  useEffect(() => () => announceHosting(null), []);

  const statuses = Object.values(state.peerStatuses);
  const aggregateBitrate = statuses.reduce(
    (total, status) => total + (status.metrics?.bitrate ?? 0),
    0,
  );
  const bitrateHistory = useBitrateHistory(
    aggregateBitrate,
    props.roomId,
    DIAGNOSTICS_HISTORY_WINDOW_MS,
  );
  const sidebarHistory = bitrateHistory.filter(
    (sample) => sample.sampledAt >= now - BITRATE_HISTORY_WINDOW_MS,
  );
  const maximumViewers = state.snapshot?.maximumViewers ?? PUBLIC_ROOM_VIEWERS;
  const parsedCapacity = Number(capacityDraft);
  const requestedProfile = initialProfile(state.captureMode);
  const directCount = statuses.filter((status) => status.metrics?.candidatePath === 'direct').length;
  const relayCount = statuses.filter((status) => status.metrics?.candidatePath === 'relay').length;
  const pathLabel = relayCount > 0 ? 'relay' : directCount > 0 ? 'direct' : 'waiting';
  const expiresIn = state.snapshot ? formatRemaining(state.snapshot.expiresAt, now) : null;
  const meta = roomMetaLine(
    props.roomId.slice(0, 8).toUpperCase(),
    pathLabel,
    approvedViewerCount + 1,
    expiresIn,
  );

  const peerRows: PeerDiagnosticsRow[] = approvedViewers.map((viewer) => {
    const status = state.peerStatuses[viewer.peerId];
    return {
      id: viewer.peerId,
      name: viewer.displayName || 'Unnamed viewer',
      path: status?.metrics?.candidatePath ?? 'unknown',
      rttMs: status?.metrics?.roundTripTimeMs ?? null,
      lossRatio: status?.metrics?.packetLossRatio ?? null,
      codec: status?.metrics?.codec ?? null,
      note: status?.lastAdaptationReason ?? null,
    };
  });

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

  const leaveRoom = () => {
    session.leave();
    void navigate('/');
  };

  if (state.ended) {
    return (
      <div className="app-shell presenter-console-shell">
        <section className="centered-state">
          <p className="eyebrow">Presenter session</p>
          <h1>This room has ended</h1>
          <p>Every viewer was disconnected and the invite link no longer works.</p>
          <a className="button button--secondary" href="/">Return home</a>
        </section>
      </div>
    );
  }

  return (
    <div className="room-shell presenter-room presenter-console-shell">
      <RoomHeader
        live={state.captureActive}
        paused={state.sharingPaused}
        title="Your room"
        meta={meta}
      >
        {state.captureActive ? (
          <>
            <Button variant="secondary" onClick={() => void session.pauseSharing()}>Pause</Button>
            <Button variant="secondary" onClick={() => void session.stopSharing()}>Stop sharing</Button>
          </>
        ) : state.sharingPaused ? (
          <>
            <Button
              variant="primary"
              onClick={() => void session.startSharing()}
              disabled={state.signaling !== 'connected'}
            >
              Resume sharing
            </Button>
            <Button variant="secondary" onClick={() => void session.stopSharing()}>Stop sharing</Button>
          </>
        ) : (
          <Button
            variant="primary"
            onClick={() => void session.startSharing()}
            disabled={state.signaling !== 'connected'}
          >
            Share my screen
          </Button>
        )}
        <Button
          variant="quiet"
          onClick={() => {
            if (theatre) setTheatreChatOpen((value) => !value);
            else setPanelTab('chat');
          }}
        >
          Chat
        </Button>
        <button
          type="button"
          className={theatre ? 'room-theatre-toggle room-theatre-toggle--active' : 'room-theatre-toggle'}
          aria-pressed={theatre}
          onClick={toggleTheatre}
        >
          Theatre <kbd>T</kbd>
        </button>
        <Button variant="quiet" onClick={leaveRoom}>Leave</Button>
        <Button variant="danger" onClick={() => setConfirmClose(true)}>Close room</Button>
      </RoomHeader>

      {confirmClose ? (
        <div className="room-confirm" role="alert">
          <span>Closing this room ends it for everyone watching.</span>
          <Button
            variant="danger"
            onClick={() => {
              setConfirmClose(false);
              session.endRoom();
            }}
          >
            Close room now
          </Button>
          <Button variant="quiet" onClick={() => setConfirmClose(false)}>Keep it open</Button>
        </div>
      ) : null}

      <div className={theatre ? 'room-layout room-layout--theatre' : 'room-layout'}>
        <div className="room-stage-column">
          <PresenterStage
            state={state}
            requestedProfile={requestedProfile}
            codecModes={codecModes}
            onSetPreferences={(preferences) => void session.setPreferences(preferences)}
            onChangeSource={() => void session.changeSource()}
          />
          {pendingViewers.length > 0 ? (
            <div className="room-prompts" role="region" aria-label="Join requests">
              {pendingViewers.map((viewer) => (
                <div className="join-prompt" key={viewer.peerId} role="alert">
                  <div className="join-prompt__who">
                    <strong>{viewer.displayName || 'Unnamed viewer'}</strong>
                    <span>
                      wants to join
                      {viewer.friendCode ? <> · <code>{viewer.friendCode}</code></> : null}
                    </span>
                  </div>
                  <div className="join-prompt__actions">
                    <Button
                      variant="primary"
                      onClick={() => session.approveViewer(viewer.peerId)}
                      disabled={approvedViewerCount >= maximumViewers}
                    >
                      Approve
                    </Button>
                    <Button variant="quiet" onClick={() => session.rejectViewer(viewer.peerId)}>
                      Deny
                    </Button>
                  </div>
                </div>
              ))}
            </div>
          ) : null}
        </div>

        {!theatre ? (
          <RoomPanel
            tabs={[
              { id: 'chat', label: 'Chat' },
              { id: 'diagnostics', label: 'Diagnostics' },
              { id: 'room', label: 'Room' },
            ]}
            active={panelTab}
            onSelect={(id) => setPanelTab(id as PanelTab)}
            meta={`${approvedViewerCount + 1} here`}
          >
            {panelTab === 'chat' ? (
              <ChatPanel log={session.chat} onSend={(text) => session.sendChat(text)} />
            ) : panelTab === 'diagnostics' ? (
              <DiagnosticsPanel
                incomingBitrate={null}
                outgoingBitrate={aggregateBitrate}
                history={bitrateHistory}
                peers={peerRows}
                onExport={downloadDiagnostics}
              />
            ) : (
              <PresenterSidebar
                snapshot={state.snapshot}
                viewerUrl={state.viewerUrl}
                copyState={copyState}
                capacityDraft={capacityDraft}
                capacityValid={isValidViewerLimit(parsedCapacity)}
                maximumViewers={maximumViewers}
                aggregateBitrate={aggregateBitrate}
                peakBitrate={Math.max(aggregateBitrate, bitratePeak(sidebarHistory))}
                bitrateHistory={sidebarHistory}
                statuses={statuses}
                directCount={directCount}
                relayCount={relayCount}
                expiresIn={expiresIn ?? 'Loading'}
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
            )}
          </RoomPanel>
        ) : null}
      </div>

      {theatre && theatreChatOpen ? (
        <TheatreChat
          log={session.chat}
          onSend={(text) => session.sendChat(text)}
          onClose={() => setTheatreChatOpen(false)}
        />
      ) : null}

      <div className="sr-only" aria-live="polite">
        {state.snapshot?.accessPolicy === 'approvalRequired'
          ? `${pendingViewers.length} viewers waiting.`
          : 'Public link admission is active.'}{' '}
        {approvedViewerCount} viewers active. Signaling {state.signaling}.
      </div>
    </div>
  );
}
