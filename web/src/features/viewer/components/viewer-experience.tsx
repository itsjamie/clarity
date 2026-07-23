import {
  useEffect,
  useMemo,
  useRef,
  useState,
  type ChangeEvent,
  type CSSProperties,
  type FormEvent,
  type KeyboardEvent,
} from 'react';
import { useNavigate } from 'react-router-dom';

import { AppHeader } from '@/components/layout/app-header';
import { Button } from '@/components/ui/button';
import { StatusPill } from '@/components/ui/status-pill';
import { useSessionState } from '@/hooks/use-session-state';
import { formatBitrate, formatPercent, formatResolution } from '@/utils/format';
import { ViewerSession } from '../session/viewer-session';

type DisplayMode = 'fit' | 'fill' | 'pixel';

export function ViewerExperience({
  roomId,
  viewerSecret,
  autoJoin,
}: {
  roomId: string;
  viewerSecret: string;
  autoJoin: boolean;
}) {
  const session = useMemo(() => new ViewerSession(roomId, viewerSecret), [roomId, viewerSecret]);
  const navigate = useNavigate();
  const state = useSessionState(session);
  const [displayNameDraft, setDisplayNameDraft] = useState('');
  const [displayMode, setDisplayMode] = useState<DisplayMode>('fit');
  const [zoom, setZoom] = useState(1);
  const [identityEditorOpen, setIdentityEditorOpen] = useState(false);
  const [diagnosticsOpen, setDiagnosticsOpen] = useState(false);
  const [muted, setMuted] = useState(true);
  const [volume, setVolume] = useState(70);
  const [isFullscreen, setIsFullscreen] = useState(false);
  const [sourceSize, setSourceSize] = useState({ width: 0, height: 0 });
  const videoRef = useRef<HTMLVideoElement>(null);
  const stageRef = useRef<HTMLDivElement>(null);
  const viewportRef = useRef<HTMLDivElement>(null);

  useEffect(() => () => session.disconnect(), [session]);
  useEffect(() => {
    if (autoJoin) session.requestAccess(null);
  }, [autoJoin, session]);
  useEffect(() => {
    if (videoRef.current && state.stream) videoRef.current.srcObject = state.stream;
  }, [state.stream]);
  useEffect(() => {
    if (!videoRef.current) return;
    videoRef.current.muted = muted;
    videoRef.current.volume = volume / 100;
  }, [muted, volume]);
  useEffect(() => {
    const updateFullscreenState = () => setIsFullscreen(document.fullscreenElement === stageRef.current);
    document.addEventListener('fullscreenchange', updateFullscreenState);
    return () => document.removeEventListener('fullscreenchange', updateFullscreenState);
  }, []);
  useEffect(() => {
    if (state.identityStatus === 'saved') setIdentityEditorOpen(false);
  }, [state.identityStatus]);

  const requestAccess = (event: FormEvent<HTMLFormElement>) => {
    event.preventDefault();
    session.requestAccess(displayNameDraft.trim() || null);
  };
  const saveDisplayName = (event: FormEvent<HTMLFormElement>) => {
    event.preventDefault();
    session.updateDisplayName(displayNameDraft.trim() || null);
  };
  const openIdentityEditor = () => {
    setDisplayNameDraft(state.displayName ?? '');
    setIdentityEditorOpen(true);
  };
  const closeIdentityEditor = () => {
    setDisplayNameDraft(state.displayName ?? '');
    setIdentityEditorOpen(false);
  };
  const toggleFullscreen = async () => {
    if (document.fullscreenElement) {
      await document.exitFullscreen?.();
    } else if (stageRef.current?.requestFullscreen) {
      await stageRef.current.requestFullscreen();
    }
  };
  const leaveSession = () => {
    session.disconnect();
    void navigate('/');
  };
  const changeVolume = (event: ChangeEvent<HTMLInputElement>) => {
    setVolume(Number(event.target.value));
    setMuted(false);
  };
  const panWithKeyboard = (event: KeyboardEvent<HTMLDivElement>) => {
    if (displayMode !== 'pixel' || !viewportRef.current) return;
    const distance = event.shiftKey ? 160 : 48;
    const directions: Partial<Record<string, [number, number]>> = {
      ArrowLeft: [-distance, 0],
      ArrowRight: [distance, 0],
      ArrowUp: [0, -distance],
      ArrowDown: [0, distance],
    };
    const delta = directions[event.key];
    if (!delta) return;
    event.preventDefault();
    viewportRef.current.scrollBy({ left: delta[0], top: delta[1], behavior: 'smooth' });
  };

  const videoStyle = {
    '--source-width': `${Math.max(sourceSize.width, 1) * zoom}px`,
    '--source-height': `${Math.max(sourceSize.height, 1) * zoom}px`,
  } as CSSProperties;
  const volumeStyle = {
    '--viewer-volume': `${muted ? 0 : volume}%`,
  } as CSSProperties;
  const terminal = ['rejected', 'kicked', 'room-ended', 'room-expired', 'failed'].includes(state.phase);
  const joining = state.phase === 'idle' || state.phase === 'connecting';
  const immersive = state.phase === 'negotiating' || state.phase === 'live';
  const identityChanged = (displayNameDraft.trim() || null) !== state.displayName;
  const pathLabel = state.metrics?.candidatePath === 'relay'
    ? 'TURN relay'
    : state.metrics?.candidatePath === 'direct'
      ? 'direct'
      : state.connectionState === 'connected'
        ? 'connected'
        : 'connecting';

  return (
    <div className={immersive ? 'app-shell viewer-shell viewer-shell--immersive' : 'app-shell viewer-shell'}>
      {!immersive ? <AppHeader /> : null}
      <main className={immersive ? 'viewer-main viewer-main--immersive' : 'viewer-main'}>
        {state.phase === 'idle' && !autoJoin ? (
          <section className="access-request" aria-labelledby="access-title">
            <p className="eyebrow">Secure room</p>
            <h1 id="access-title">Join the share.</h1>
            <p>Choose a label so the presenter can recognize this browser. Some rooms require approval.</p>
            <form onSubmit={requestAccess}>
              <label className="field">
                <span>Display name <small>optional</small></span>
                <input
                  value={displayNameDraft}
                  maxLength={48}
                  autoComplete="name"
                  onChange={(event) => setDisplayNameDraft(event.target.value)}
                  placeholder="How should the presenter recognize you?"
                />
              </label>
              <Button variant="primary" type="submit">Join room</Button>
            </form>
            <div className="privacy-note">
              <strong>Before you continue</strong>
              <span>A direct WebRTC connection may reveal network endpoints to the presenter.</span>
            </div>
          </section>
        ) : null}

        {!terminal && (state.phase !== 'idle' || autoJoin) ? (
          <section className={immersive ? 'viewer-session viewer-session--immersive' : 'viewer-session'}>
            {!immersive ? (
              <header className="viewer-session__header">
                <div>
                  <StatusPill tone={state.phase === 'live' ? 'success' : state.signaling === 'reconnecting' ? 'warning' : 'accent'}>
                    {state.phase === 'awaiting-approval' ? 'Awaiting approval' : joining ? 'connecting' : state.phase}
                  </StatusPill>
                  <span className="room-label">Room {roomId.slice(0, 8)}</span>
                </div>
                <div className="viewer-header__states">
                  <span>Signaling: {state.signaling}</span>
                  <span>Media: {state.connectionState}</span>
                </div>
              </header>
            ) : null}

            {state.phase === 'awaiting-approval' || joining ? (
              <div className="waiting-state" role="status">
                <div className="waiting-state__signal" aria-hidden="true"><span /><span /><span /></div>
                <h1>{joining ? 'Joining the room' : 'Waiting for the presenter'}</h1>
                <p>
                  {joining
                    ? 'Authenticating this secure invitation.'
                    : 'No SDP or media is exchanged until the presenter approves you.'}
                </p>
              </div>
            ) : (
              <div className="viewer-stage" ref={stageRef} onDoubleClick={() => void toggleFullscreen()}>
                <div
                  className={`video-viewport video-viewport--${displayMode}`}
                  ref={viewportRef}
                  tabIndex={displayMode === 'pixel' ? 0 : -1}
                  onKeyDown={panWithKeyboard}
                  aria-label={displayMode === 'pixel' ? 'Shared screen viewport. Use arrow keys to pan.' : 'Shared screen viewport'}
                >
                  <video
                    ref={videoRef}
                    className={`shared-video shared-video--${displayMode}`}
                    style={videoStyle}
                    autoPlay
                    playsInline
                    muted={muted}
                    onLoadedMetadata={(event) =>
                      setSourceSize({ width: event.currentTarget.videoWidth, height: event.currentTarget.videoHeight })
                    }
                  />
                  {state.snapshot?.sharingState === 'paused' ? (
                    <SharingPausedSlate />
                  ) : !state.stream ? (
                    <div className="negotiating-overlay">Negotiating secure media…</div>
                  ) : null}
                </div>

                {diagnosticsOpen ? (
                  <aside
                    className="quality-hud"
                    id="viewer-diagnostics"
                    aria-label="Incoming stream quality"
                    onDoubleClick={(event) => event.stopPropagation()}
                  >
                    <button
                      type="button"
                      className="quality-hud__close"
                      aria-label="Close diagnostics"
                      onClick={() => setDiagnosticsOpen(false)}
                    >
                      <span aria-hidden="true">×</span>
                    </button>
                    <div><span>Incoming</span><strong>{formatBitrate(state.metrics?.bitrate)}</strong></div>
                    <div><span>Source</span><strong>{formatResolution(state.metrics?.frameWidth, state.metrics?.frameHeight)}</strong></div>
                    <div><span>Frame rate</span><strong>{state.metrics?.framesPerSecond ? `${state.metrics.framesPerSecond.toFixed(0)} FPS` : 'Unavailable'}</strong></div>
                    <div><span>Codec</span><strong>{state.metrics?.codec ?? 'Unavailable'}</strong></div>
                    <div><span>Packet loss</span><strong>{formatPercent(state.metrics?.packetLossRatio)}</strong></div>
                    <div><span>Path</span><strong>{state.metrics?.candidatePath === 'relay' ? 'TURN relay' : titleCase(state.metrics?.candidatePath ?? 'unavailable')}</strong></div>
                  </aside>
                ) : null}

                <div className="viewer-controls-zone" onDoubleClick={(event) => event.stopPropagation()}>
                  <div className="viewer-toolbar" role="toolbar" aria-label="Viewing controls">
                    <div className="viewer-room-status" aria-label={`Room ${roomId.slice(0, 8)}, ${pathLabel}`}>
                      <span
                        className={state.phase === 'live' && !state.presenterDisconnected
                          ? 'viewer-room-status__dot viewer-room-status__dot--live'
                          : 'viewer-room-status__dot'}
                        aria-hidden="true"
                      />
                      <span className="viewer-room-status__id">{roomId.slice(0, 8).toUpperCase()}</span>
                      <span className="viewer-room-status__path" aria-hidden="true">· {pathLabel}</span>
                    </div>

                    <span className="viewer-toolbar__separator" aria-hidden="true" />

                    <div className="display-modes" role="group" aria-label="Display mode">
                      {(['fit', 'fill', 'pixel'] as const).map((mode) => (
                        <button
                          key={mode}
                          type="button"
                          className={displayMode === mode ? 'toolbar-button toolbar-button--active' : 'toolbar-button'}
                          aria-pressed={displayMode === mode}
                          onClick={() => setDisplayMode(mode)}
                        >
                          {mode === 'pixel' ? '1:1' : titleCase(mode)}
                        </button>
                      ))}
                    </div>

                    {displayMode === 'pixel' ? (
                      <div className="viewer-zoom-control" role="group" aria-label="Pixel zoom">
                        <button
                          type="button"
                          className="viewer-icon-button viewer-zoom-control__button"
                          aria-label="Zoom out"
                          disabled={zoom <= 0.5}
                          onClick={() => setZoom((value) => Math.max(0.5, value - 0.25))}
                        >
                          <span aria-hidden="true">−</span>
                        </button>
                        <output aria-live="polite">{Math.round(zoom * 100)}%</output>
                        <button
                          type="button"
                          className="viewer-icon-button viewer-zoom-control__button"
                          aria-label="Zoom in"
                          disabled={zoom >= 2}
                          onClick={() => setZoom((value) => Math.min(2, value + 0.25))}
                        >
                          <span aria-hidden="true">+</span>
                        </button>
                      </div>
                    ) : null}

                    <span className="viewer-toolbar__separator" aria-hidden="true" />

                    <div className="viewer-audio-controls">
                      <button
                        type="button"
                        className="viewer-icon-button"
                        aria-label={muted ? 'Unmute shared audio' : 'Mute shared audio'}
                        onClick={() => setMuted((value) => !value)}
                      >
                        <VolumeIcon muted={muted} />
                      </button>
                      <input
                        className="viewer-volume"
                        type="range"
                        min="0"
                        max="100"
                        value={volume}
                        style={volumeStyle}
                        aria-label="Shared audio volume"
                        onChange={changeVolume}
                      />
                    </div>

                    <span className="viewer-toolbar__separator" aria-hidden="true" />

                    <button
                      type="button"
                      className="viewer-toolbar__text-button"
                      aria-expanded={diagnosticsOpen}
                      aria-controls="viewer-diagnostics"
                      onClick={() => setDiagnosticsOpen((value) => !value)}
                    >
                      {diagnosticsOpen ? 'Hide diagnostics' : 'Diagnostics'}
                    </button>

                    <span className="viewer-toolbar__separator" aria-hidden="true" />

                    <div className={identityEditorOpen ? 'viewer-identity-control viewer-identity-control--open' : 'viewer-identity-control'}>
                      {identityEditorOpen ? (
                        <form className="viewer-identity-form" onSubmit={saveDisplayName}>
                          <input
                            autoFocus
                            aria-label="Name shown to presenter"
                            aria-invalid={state.identityStatus === 'failed'}
                            value={displayNameDraft}
                            maxLength={48}
                            autoComplete="name"
                            placeholder="Your name"
                            onChange={(event) => setDisplayNameDraft(event.target.value)}
                          />
                          <button
                            type="submit"
                            className="viewer-toolbar__text-button viewer-identity-form__save"
                            disabled={!identityChanged || state.identityStatus === 'saving'}
                          >
                            {state.identityStatus === 'saving' ? 'Saving…' : 'Save'}
                          </button>
                          <button
                            type="button"
                            className="viewer-icon-button viewer-identity-form__cancel"
                            aria-label="Cancel name edit"
                            onClick={closeIdentityEditor}
                          >
                            <span aria-hidden="true">×</span>
                          </button>
                          {state.identityError ? (
                            <span className="viewer-identity-error" role="alert">{state.identityError}</span>
                          ) : null}
                        </form>
                      ) : (
                        <button
                          type="button"
                          className="viewer-toolbar__text-button viewer-identity-trigger"
                          aria-label={state.displayName ? 'Edit viewer name' : 'Set viewer name'}
                          onClick={openIdentityEditor}
                        >
                          <span>{state.displayName || 'Set name'}</span>
                        </button>
                      )}
                    </div>

                    <span className="viewer-toolbar__spacer" />

                    <button
                      type="button"
                      className="viewer-icon-button"
                      aria-label={isFullscreen ? 'Exit fullscreen' : 'Enter fullscreen'}
                      onClick={() => void toggleFullscreen()}
                    >
                      <FullscreenIcon active={isFullscreen} />
                    </button>

                    <span className="viewer-toolbar__separator" aria-hidden="true" />

                    <button type="button" className="viewer-toolbar__text-button viewer-toolbar__leave" onClick={leaveSession}>
                      Leave
                    </button>
                  </div>
                </div>

                {state.presenterDisconnected ? (
                  <p className="signaling-banner signaling-banner--overlay" role="status">
                    Presenter signaling is reconnecting. Healthy media remains open.
                  </p>
                ) : null}
              </div>
            )}

            {state.presenterDisconnected && !immersive ? (
              <p className="signaling-banner" role="status">Presenter signaling is reconnecting. Healthy media remains open.</p>
            ) : null}
          </section>
        ) : null}

        {terminal ? <TerminalState phase={state.phase} error={state.error} /> : null}
      </main>
      <div className="sr-only" aria-live="polite">
        Viewer state {state.phase}. Signaling {state.signaling}. Connection {state.connectionState}.
      </div>
    </div>
  );
}

function TerminalState({ phase, error }: { phase: string; error: string | null }) {
  const content: Record<string, [string, string]> = {
    rejected: ['Access was not approved', 'The presenter rejected this request.'],
    kicked: ['You were removed', 'The presenter ended this viewer connection.'],
    'room-ended': ['The share has ended', 'The presenter closed the room.'],
    'room-expired': ['This room expired', 'Ask the presenter for a new secure invitation.'],
    failed: ['The connection could not continue', error ?? 'Check the invitation and try again.'],
  };
  const [title, message] = content[phase] ?? ['Session ended', 'This viewer session is no longer active.'];
  return (
    <section className="centered-state">
      <p className="eyebrow">Viewer session</p>
      <h1>{title}</h1>
      <p>{message}</p>
      <a className="button button--secondary" href="/">Return home</a>
    </section>
  );
}

function SharingPausedSlate() {
  return (
    <div className="viewer-sharing-paused" role="status" aria-live="polite">
      <span className="viewer-sharing-paused__icon" aria-hidden="true">
        <i />
        <i />
      </span>
      <strong>Sharing paused</strong>
      <span>The presenter will resume with a new source.</span>
    </div>
  );
}

function titleCase(value: string): string {
  return value.charAt(0).toUpperCase() + value.slice(1);
}

function VolumeIcon({ muted }: { muted: boolean }) {
  return (
    <svg width="18" height="18" viewBox="0 0 24 24" aria-hidden="true">
      <path d="M3 9v6h5l5 5V4L8 9H3Z" fill="currentColor" />
      {muted ? (
        <path d="m16.5 8 4 8M20.5 8l-4 8" fill="none" stroke="currentColor" strokeWidth="1.6" strokeLinecap="round" />
      ) : (
        <path d="M16.2 8.4a5 5 0 0 1 0 7.2M18.8 6a8.3 8.3 0 0 1 0 12" fill="none" stroke="currentColor" strokeWidth="1.6" strokeLinecap="round" />
      )}
    </svg>
  );
}

function FullscreenIcon({ active }: { active: boolean }) {
  return active ? (
    <svg width="18" height="18" viewBox="0 0 24 24" aria-hidden="true">
      <path d="M9 4v5H4M15 4v5h5M9 20v-5H4M15 20v-5h5" fill="none" stroke="currentColor" strokeWidth="1.8" strokeLinecap="round" strokeLinejoin="round" />
    </svg>
  ) : (
    <svg width="18" height="18" viewBox="0 0 24 24" aria-hidden="true">
      <path d="M9 4H4v5M15 4h5v5M9 20H4v-5M15 20h5v-5" fill="none" stroke="currentColor" strokeWidth="1.8" strokeLinecap="round" strokeLinejoin="round" />
    </svg>
  );
}
