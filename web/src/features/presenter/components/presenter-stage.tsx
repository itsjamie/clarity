import { Button } from '@/components/ui/button';
import type { CodecMode } from '@/lib/webrtc/codec-capability-service';
import type { CaptureMode, EncodingProfile, QualityStrategy } from '@/lib/webrtc/profiles';
import { formatResolution } from '@/utils/format';
import type { PresenterSessionState } from '../session/presenter-session';
import { LocalStreamPreview } from './local-stream-preview';

interface PresenterStageProps {
  state: PresenterSessionState;
  requestedProfile: EncodingProfile;
  codecModes: readonly CodecMode[];
  confirmEnd: boolean;
  onSetPreferences: (preferences: {
    captureMode?: CaptureMode;
    qualityStrategy?: QualityStrategy;
    codecMode?: CodecMode;
    audioRequested?: boolean;
  }) => void;
  onStartSharing: () => void;
  onChangeSource: () => void;
  onRequestEnd: () => void;
  onCancelEnd: () => void;
  onEndRoom: () => void;
}

export function PresenterStage({
  state,
  requestedProfile,
  codecModes,
  confirmEnd,
  onSetPreferences,
  onStartSharing,
  onChangeSource,
  onRequestEnd,
  onCancelEnd,
  onEndRoom,
}: PresenterStageProps) {
  const modeDescription = state.captureMode === 'text' ? 'Text & documents' : 'Motion & video';
  const resolution = state.captureActive
    ? formatResolution(state.captureSettings?.width, state.captureSettings?.height)
    : '2560 × 1440 target';
  const frameRate = state.captureActive
    ? state.captureSettings?.frameRate?.toFixed(0) ?? 'Unknown'
    : String(requestedProfile.maxFramerate);
  const statusLabel = state.ended
    ? 'Room ended'
    : state.captureActive
      ? "You're sharing your screen"
      : state.signaling === 'connected'
        ? 'Ready to share'
        : 'Connecting to room';

  return (
    <section className="presenter-stage" aria-labelledby="presenter-stage-status">
      <header className="presenter-stage__header">
        <div className="presenter-stage__status heading-status">
          <i
            className={`presenter-stage__status-dot${state.captureActive ? ' presenter-stage__status-dot--live' : ''}`}
            aria-hidden="true"
          />
          <strong id="presenter-stage-status">{statusLabel}</strong>
          <span aria-hidden="true">·</span>
          <span>{modeDescription} · {resolution} · {frameRate} FPS</span>
        </div>
        {state.captureActive ? (
          <Button variant="danger" onClick={onRequestEnd} disabled={state.ended}>
            Stop sharing
          </Button>
        ) : (
          <Button
            variant="primary"
            onClick={onStartSharing}
            disabled={state.signaling !== 'connected' || state.ended}
          >
            Start sharing
          </Button>
        )}
      </header>

      {confirmEnd ? (
        <div className="presenter-stage__confirm" role="alert">
          <span>Stopping the share ends this room for every viewer.</span>
          <Button variant="danger" onClick={onEndRoom}>End room now</Button>
          <Button variant="quiet" onClick={onCancelEnd}>Keep sharing</Button>
        </div>
      ) : null}

      <LocalStreamPreview stream={state.previewStream} active={state.captureActive} />

      <div className="presenter-stage__toolbar" aria-label="Sharing controls">
        <fieldset className="presenter-stage__mode" disabled={state.ended}>
          <legend className="sr-only">Capture mode</legend>
          <button
            type="button"
            className={state.captureMode === 'text' ? 'is-active' : ''}
            aria-pressed={state.captureMode === 'text'}
            onClick={() => onSetPreferences({ captureMode: 'text' })}
          >
            Text
          </button>
          <button
            type="button"
            className={state.captureMode === 'motion' ? 'is-active' : ''}
            aria-pressed={state.captureMode === 'motion'}
            onClick={() => onSetPreferences({ captureMode: 'motion' })}
          >
            Motion
          </button>
        </fieldset>

        <label className="presenter-stage__audio">
          <input
            type="checkbox"
            checked={state.audioRequested}
            onChange={(event) => onSetPreferences({ audioRequested: event.target.checked })}
            disabled={state.captureActive || state.ended}
          />
          <span>Share audio</span>
        </label>

        <label className="presenter-stage__select">
          <span className="sr-only">Codec</span>
          <select
            aria-label="Codec"
            value={state.codecMode}
            onChange={(event) => onSetPreferences({ codecMode: event.target.value as CodecMode })}
            disabled={state.ended}
          >
            {codecModes.map((mode) => (
              <option key={mode} value={mode}>{mode === 'auto' ? 'Auto codec' : mode}</option>
            ))}
          </select>
        </label>

        <label className="presenter-stage__select presenter-stage__select--quality">
          <span className="sr-only">Quality control</span>
          <select
            aria-label="Quality control"
            value={state.qualityStrategy}
            onChange={(event) => onSetPreferences({ qualityStrategy: event.target.value as QualityStrategy })}
            disabled={state.ended}
          >
            <option value="adaptive">Adaptive quality</option>
            <option value="fixed">Locked to high</option>
          </select>
        </label>

        {state.captureActive ? (
          <Button className="presenter-stage__change-source" variant="primary" onClick={onChangeSource}>
            Change source
          </Button>
        ) : null}
      </div>

      <div className="presenter-stage__footer">
        <span>{resolution}</span>
        <span aria-hidden="true">·</span>
        <span>{frameRate} FPS</span>
        <span aria-hidden="true">·</span>
        <span>{(requestedProfile.maxBitrate / 1_000_000).toFixed(1)} Mbps ceiling</span>
      </div>

      <div className="presenter-stage__notices" aria-live="polite">
        {state.warning ? <p className="inline-notice inline-notice--warning">{state.warning}</p> : null}
        {state.error ? <p className="inline-notice inline-notice--danger">{state.error}</p> : null}
      </div>
    </section>
  );
}
