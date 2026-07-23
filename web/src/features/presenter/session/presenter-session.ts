import type {
  ClientMessage,
  IceConfiguration,
  RoomSnapshot,
  ServerMessage,
  SharingState,
} from '@/generated/protocol';
import { PROTOCOL_VERSION, signalingUrl } from '@/config/environment';
import { DiagnosticsCollector } from '@/lib/diagnostics/diagnostics-collector';
import {
  SignalingClient,
  type SignalingState,
} from '@/lib/signaling/signaling-client';
import type { CodecMode } from '@/lib/webrtc/codec-capability-service';
import type { CaptureMode, QualityStrategy } from '@/lib/webrtc/profiles';
import type { ExternalStateStore } from '@/hooks/use-session-state';
import {
  ScreenCaptureManager,
  type CaptureSettings,
} from '../media/screen-capture-manager';
import {
  PresenterConnectionManager,
  type PresenterPeerStatus,
} from '../webrtc/presenter-connection-manager';
import { withPendingViewer, withResumedViewer } from './presenter-snapshot';

export interface PresenterSessionState {
  signaling: SignalingState;
  snapshot: RoomSnapshot | null;
  captureActive: boolean;
  sharingPaused: boolean;
  previewStream: MediaStream | null;
  captureSettings: CaptureSettings | null;
  captureMode: CaptureMode;
  qualityStrategy: QualityStrategy;
  codecMode: CodecMode;
  audioRequested: boolean;
  viewerUrl: string;
  peerStatuses: Readonly<Record<string, PresenterPeerStatus>>;
  warning: string | null;
  error: string | null;
  ended: boolean;
}

export class PresenterSession implements ExternalStateStore<PresenterSessionState> {
  readonly #listeners = new Set<() => void>();
  readonly #diagnostics = new DiagnosticsCollector();
  readonly #signaling: SignalingClient;
  readonly #connections: PresenterConnectionManager;
  readonly #capture: ScreenCaptureManager;
  #iceConfiguration: IceConfiguration | null = null;
  #pausing = false;
  #starting = false;
  #captureOperationRevision = 0;
  #sharingStateToSync: SharingState | null = null;
  #state: PresenterSessionState;

  public constructor(roomId: string, presenterSecret: string, viewerUrl: string) {
    this.#state = {
      signaling: 'idle',
      snapshot: null,
      captureActive: false,
      sharingPaused: false,
      previewStream: null,
      captureSettings: null,
      captureMode: 'text',
      qualityStrategy: 'adaptive',
      codecMode: 'auto',
      audioRequested: true,
      viewerUrl,
      peerStatuses: {},
      warning: null,
      error: null,
      ended: false,
    };
    this.#signaling = new SignalingClient({
      url: signalingUrl(),
      roomId,
      role: 'presenter',
      authentication: {
        type: 'auth:presenter',
        protocolVersion: PROTOCOL_VERSION,
        requestId: crypto.randomUUID(),
        roomId,
        presenterSecret,
      },
      onMessage: (message) => void this.#handleMessage(message),
      onStateChange: (signaling) => {
        this.#patch({ signaling });
        this.#diagnostics.record('signaling.state', { state: signaling });
      },
    });
    this.#connections = new PresenterConnectionManager({
      sendSignal: (message) => this.#signaling.send(message),
      onStatus: (status) => {
        this.#patch({
          peerStatuses: { ...this.#state.peerStatuses, [status.peerId]: status },
        });
      },
      diagnostics: this.#diagnostics,
    });
    this.#capture = new ScreenCaptureManager(() => void this.pauseSharing());
  }

  public getSnapshot = (): PresenterSessionState => this.#state;

  public subscribe = (listener: () => void): (() => void) => {
    this.#listeners.add(listener);
    return () => this.#listeners.delete(listener);
  };

  public connect(): void {
    this.#signaling.connect();
  }

  public async setPreferences(preferences: {
    captureMode?: CaptureMode;
    qualityStrategy?: QualityStrategy;
    codecMode?: CodecMode;
    audioRequested?: boolean;
  }): Promise<void> {
    if (this.#state.captureActive && preferences.captureMode) {
      this.#patch({ warning: 'Capture mode changes apply when you change the source.' });
    }
    this.#patch(preferences);
    if (this.#iceConfiguration) {
      await this.#connections.configure(
        this.#iceConfiguration,
        preferences.captureMode ?? this.#state.captureMode,
        preferences.qualityStrategy ?? this.#state.qualityStrategy,
        preferences.codecMode ?? this.#state.codecMode,
      );
    }
  }

  public async startSharing(): Promise<void> {
    if (
      this.#state.captureActive ||
      this.#state.ended ||
      this.#starting ||
      this.#pausing
    ) return;
    this.#starting = true;
    const operationRevision = ++this.#captureOperationRevision;
    const resuming = this.#state.sharingPaused;
    let sharingStateUpdateAttempted = false;
    this.#patch({ error: null, warning: null });
    try {
      const result = await this.#capture.start(
        this.#state.captureMode,
        this.#state.audioRequested,
      );
      if (!this.#isCurrentCaptureOperation(operationRevision)) {
        this.#capture.stop();
        return;
      }
      if (!this.#iceConfiguration) throw new Error('Signaling authentication is not complete.');
      await this.#connections.configure(
        this.#iceConfiguration,
        this.#state.captureMode,
        this.#state.qualityStrategy,
        this.#state.codecMode,
      );
      if (!this.#isCurrentCaptureOperation(operationRevision)) {
        this.#capture.stop();
        return;
      }
      if (resuming) {
        const failures = await this.#connections.replaceSource(result.stream);
        if (!this.#isCurrentCaptureOperation(operationRevision)) {
          this.#capture.stop();
          return;
        }
        if (failures.length > 0) {
          throw new Error(`Could not resume sharing for ${failures.length} viewer connection(s).`);
        }
      }
      await this.#connections.setSource(result.stream);
      try {
        sharingStateUpdateAttempted = true;
        this.#updateSharingState('live');
      } catch (error) {
        await this.#connections.pauseSource();
        throw error;
      }
      this.#patch({
        captureActive: true,
        sharingPaused: false,
        previewStream: result.stream,
        captureSettings: result.settings,
        warning: result.audioWarning ?? null,
      });
      this.#diagnostics.record(resuming ? 'capture.resumed' : 'capture.started', result.settings);
    } catch (error) {
      this.#capture.stop();
      if (sharingStateUpdateAttempted) this.#sharingStateToSync = null;
      if (!this.#isCurrentCaptureOperation(operationRevision)) return;
      this.#patch({
        error: errorMessage(error),
        captureActive: false,
        sharingPaused: resuming,
        previewStream: null,
        captureSettings: null,
      });
    } finally {
      this.#starting = false;
    }
  }

  public async pauseSharing(): Promise<void> {
    if (
      !this.#state.captureActive ||
      this.#state.ended ||
      this.#pausing ||
      this.#starting
    ) return;
    this.#pausing = true;
    const operationRevision = ++this.#captureOperationRevision;
    let sharingStateUpdateAttempted = false;
    try {
      sharingStateUpdateAttempted = true;
      this.#updateSharingState('paused');
      const failures = await this.#connections.pauseSource();
      if (!this.#isCurrentCaptureOperation(operationRevision)) {
        return;
      }
      this.#capture.stop();
      this.#patch({
        captureActive: false,
        sharingPaused: true,
        previewStream: null,
        captureSettings: null,
        warning: failures.length > 0
          ? `Sharing paused, but ${failures.length} viewer connection(s) could not pause cleanly.`
          : null,
        error: null,
      });
      this.#diagnostics.record('capture.paused', { replacementFailures: failures.length });
    } catch (error) {
      if (!this.#isCurrentCaptureOperation(operationRevision)) return;
      const captureEnded = this.#capture.stream
        ?.getVideoTracks()
        .every((track) => track.readyState === 'ended') ?? false;
      if (sharingStateUpdateAttempted && !captureEnded) {
        this.#sharingStateToSync = null;
      }
      if (captureEnded) {
        this.#capture.stop();
        this.#patch({
          captureActive: false,
          sharingPaused: true,
          previewStream: null,
          captureSettings: null,
          error: errorMessage(error),
        });
      } else {
        this.#patch({ error: errorMessage(error) });
      }
    } finally {
      this.#pausing = false;
    }
  }

  public async changeSource(): Promise<void> {
    try {
      const result = await this.#capture.changeSource(
        this.#state.captureMode,
        this.#state.audioRequested,
        (stream) => this.#connections.replaceSource(stream),
      );
      this.#patch({
        previewStream: result.stream,
        captureSettings: result.settings,
        warning: result.audioWarning ?? null,
      });
      this.#diagnostics.record('capture.changed', result.settings);
    } catch (error) {
      this.#patch({ error: errorMessage(error) });
    }
  }

  public approveViewer(peerId: string): void {
    this.#send({
      type: 'viewer:approve',
      protocolVersion: PROTOCOL_VERSION,
      requestId: crypto.randomUUID(),
      peerId,
    });
  }

  public rejectViewer(peerId: string): void {
    this.#send({
      type: 'viewer:reject',
      protocolVersion: PROTOCOL_VERSION,
      requestId: crypto.randomUUID(),
      peerId,
    });
  }

  public kickViewer(peerId: string): void {
    this.#send({
      type: 'viewer:kick',
      protocolVersion: PROTOCOL_VERSION,
      requestId: crypto.randomUUID(),
      peerId,
    });
    this.#connections.removeViewer(peerId);
  }

  public updateCapacity(maximumViewers: number): void {
    this.#send({
      type: 'room:update-capacity',
      protocolVersion: PROTOCOL_VERSION,
      requestId: crypto.randomUUID(),
      maximumViewers,
    });
  }

  public async restartViewerIce(peerId: string): Promise<void> {
    await this.#connections.restartIce(peerId);
  }

  public endRoom(): void {
    if (this.#state.ended) return;
    this.#captureOperationRevision += 1;
    this.#sharingStateToSync = null;
    this.#capture.stop();
    this.#connections.stopAll();
    try {
      this.#send({
        type: 'room:close',
        protocolVersion: PROTOCOL_VERSION,
        requestId: crypto.randomUUID(),
      });
    } catch {
      // Local media cleanup is authoritative even if signaling is already gone.
    }
    this.#signaling.disconnect(false);
    this.#patch({
      captureActive: false,
      sharingPaused: false,
      previewStream: null,
      captureSettings: null,
      ended: true,
    });
  }

  public disconnect(): void {
    this.#captureOperationRevision += 1;
    this.#sharingStateToSync = null;
    this.#capture.stop();
    this.#connections.stopAll();
    this.#signaling.disconnect();
  }

  public diagnosticsJson(): string {
    return JSON.stringify(
      this.#diagnostics.export({
        protocolVersion: PROTOCOL_VERSION,
        signaling: this.#state.signaling,
        captureSettings: this.#state.captureSettings,
        roomExpiresAt: this.#state.snapshot?.expiresAt,
        peerCount: Object.keys(this.#state.peerStatuses).length,
      }),
      null,
      2,
    );
  }

  async #handleMessage(message: ServerMessage): Promise<void> {
    switch (message.type) {
      case 'auth:succeeded': {
        const sharingStateToSync = this.#sharingStateToSync;
        this.#iceConfiguration = message.iceConfiguration;
        await this.#connections.configure(
          message.iceConfiguration,
          this.#state.captureMode,
          this.#state.qualityStrategy,
          this.#state.codecMode,
        );
        await this.#applySnapshot(message.snapshot);
        if (sharingStateToSync) {
          this.#patch({ sharingPaused: sharingStateToSync === 'paused' });
          try {
            this.#updateSharingState(sharingStateToSync);
          } catch {
            // The desired state remains queued for the next successful resume.
          }
        }
        break;
      }
      case 'room:snapshot':
        await this.#applySnapshot(message.snapshot);
        break;
      case 'room:capacity-updated':
        if (this.#state.snapshot) {
          this.#patch({
            snapshot: { ...this.#state.snapshot, maximumViewers: message.maximumViewers },
          });
        }
        break;
      case 'room:sharing-state-updated':
        if (this.#sharingStateToSync === message.sharingState) {
          this.#sharingStateToSync = null;
        }
        this.#patch({
          sharingPaused: message.sharingState === 'paused',
          snapshot: this.#state.snapshot
            ? { ...this.#state.snapshot, sharingState: message.sharingState }
            : null,
        });
        break;
      case 'viewer:left':
      case 'viewer:kicked':
        this.#connections.removeViewer(message.peerId);
        this.#removePeerStatus(message.peerId);
        break;
      case 'viewer:pending':
        if (this.#state.snapshot) {
          this.#patch({ snapshot: withPendingViewer(this.#state.snapshot, message.viewer) });
        }
        break;
      case 'viewer:resumed':
        if (this.#state.snapshot) {
          this.#patch({ snapshot: withResumedViewer(this.#state.snapshot, message.peerId) });
        }
        break;
      case 'signal:answer':
        await this.#connections.applyAnswer(message.sourcePeerId, message.sdp);
        break;
      case 'signal:ice-candidate':
        await this.#connections.addRemoteCandidate(message.sourcePeerId, {
          candidate: message.candidate,
          sdpMid: message.sdpMid,
          sdpMLineIndex: message.sdpMLineIndex,
        });
        break;
      case 'signal:ice-restart':
        await this.#connections.restartIce(message.sourcePeerId);
        break;
      case 'ice:configuration':
        this.#iceConfiguration = message.configuration;
        await this.#connections.configure(
          message.configuration,
          this.#state.captureMode,
          this.#state.qualityStrategy,
          this.#state.codecMode,
        );
        break;
      case 'room:closed':
      case 'room:expired':
        this.#captureOperationRevision += 1;
        this.#sharingStateToSync = null;
        this.#capture.stop();
        this.#connections.stopAll();
        this.#patch({
          captureActive: false,
          sharingPaused: false,
          previewStream: null,
          captureSettings: null,
          ended: true,
        });
        break;
      case 'error':
      case 'auth:failed':
        this.#patch({ error: message.message });
        break;
      case 'viewer:approved':
      case 'viewer:rejected':
      case 'presenter:disconnected':
      case 'presenter:resumed':
      case 'signal:offer':
        break;
    }
  }

  async #applySnapshot(snapshot: RoomSnapshot): Promise<void> {
    this.#patch({
      snapshot,
      sharingPaused: snapshot.sharingState === 'paused',
    });
    const approved = new Set(snapshot.approvedViewers.map((viewer) => viewer.peerId));
    for (const viewer of snapshot.approvedViewers) {
      await this.#connections.addApprovedViewer(viewer.peerId);
    }
    for (const status of this.#connections.statuses) {
      if (!approved.has(status.peerId)) {
        this.#connections.removeViewer(status.peerId);
        this.#removePeerStatus(status.peerId);
      }
    }
  }

  #send(message: ClientMessage): void {
    this.#signaling.send(message);
  }

  #removePeerStatus(peerId: string): void {
    const next = { ...this.#state.peerStatuses };
    delete next[peerId];
    this.#patch({ peerStatuses: next });
  }

  #isCurrentCaptureOperation(revision: number): boolean {
    return revision === this.#captureOperationRevision && !this.#state.ended;
  }

  #updateSharingState(sharingState: SharingState): void {
    this.#sharingStateToSync = sharingState;
    this.#send({
      type: 'room:update-sharing-state',
      protocolVersion: PROTOCOL_VERSION,
      requestId: crypto.randomUUID(),
      sharingState,
    });
  }

  #patch(patch: Partial<PresenterSessionState>): void {
    this.#state = { ...this.#state, ...patch };
    this.#listeners.forEach((listener) => listener());
  }
}

function errorMessage(error: unknown): string {
  return error instanceof Error ? error.message : 'The operation could not be completed.';
}
