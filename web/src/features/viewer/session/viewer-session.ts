import type {
  ClientMessage,
  RoomSnapshot,
  ServerMessage,
  SharingState,
} from '@/generated/protocol';
import { PROTOCOL_VERSION, signalingUrl } from '@/config/environment';
import type { ExternalStateStore } from '@/hooks/use-session-state';
import { ChatLog } from '@/lib/chat/chat-log';
import { DiagnosticsCollector } from '@/lib/diagnostics/diagnostics-collector';
import { bytesToBase64 } from '@/lib/identity/identity-store';
import { identityStore } from '@/lib/presence/presence-service';
import {
  SignalingClient,
  type SignalingState,
} from '@/lib/signaling/signaling-client';
import { IceRefreshScheduler } from '@/lib/webrtc/ice-refresh-scheduler';
import { IceRestartScheduler } from '@/lib/webrtc/ice-restart-scheduler';
import type { WebRtcMetrics } from '@/lib/webrtc/stats-collector';
import { ViewerPeerConnection } from '../webrtc/viewer-peer-connection';

export type ViewerSessionPhase =
  | 'idle'
  | 'connecting'
  | 'awaiting-approval'
  | 'negotiating'
  | 'live'
  | 'rejected'
  | 'kicked'
  | 'room-ended'
  | 'room-expired'
  | 'failed';

export interface ViewerSessionState {
  signaling: SignalingState;
  phase: ViewerSessionPhase;
  snapshot: RoomSnapshot | null;
  stream: MediaStream | null;
  metrics: WebRtcMetrics | null;
  connectionState: RTCPeerConnectionState | 'new';
  iceState: RTCIceConnectionState | 'new';
  presenterDisconnected: boolean;
  displayName: string | null;
  identityStatus: 'idle' | 'saving' | 'saved' | 'failed';
  identityError: string | null;
  error: string | null;
}

export class ViewerSession implements ExternalStateStore<ViewerSessionState> {
  public readonly chat = new ChatLog();
  readonly #listeners = new Set<() => void>();
  readonly #diagnostics = new DiagnosticsCollector();
  readonly #roomId: string;
  readonly #viewerSecret: string;
  readonly #recovery = new IceRestartScheduler({
    requestRestart: () => this.#requestIceRestart(),
  });
  readonly #credentialRefresh = new IceRefreshScheduler({
    requestRefresh: () => this.#requestIceRefresh(),
  });
  #signaling: SignalingClient | null = null;
  #peer: ViewerPeerConnection | null = null;
  #selfPeerId: string | null = null;
  #presenterPeerId: string | null = null;
  #identityRequestId: string | null = null;
  readonly #signalRequestIds = new Set<string>();
  #state: ViewerSessionState = {
    signaling: 'idle',
    phase: 'idle',
    snapshot: null,
    stream: null,
    metrics: null,
    connectionState: 'new',
    iceState: 'new',
    presenterDisconnected: false,
    displayName: null,
    identityStatus: 'idle',
    identityError: null,
    error: null,
  };

  public constructor(roomId: string, viewerSecret: string) {
    this.#roomId = roomId;
    this.#viewerSecret = viewerSecret;
  }

  public getSnapshot = (): ViewerSessionState => this.#state;

  public subscribe = (listener: () => void): (() => void) => {
    this.#listeners.add(listener);
    return () => this.#listeners.delete(listener);
  };

  public requestAccess(displayName: string | null): void {
    if (this.#signaling) return;
    this.#patch({ phase: 'connecting', error: null });
    this.#signaling = new SignalingClient({
      url: signalingUrl(),
      roomId: this.#roomId,
      role: 'viewer',
      authentication: {
        type: 'auth:viewer',
        protocolVersion: PROTOCOL_VERSION,
        requestId: crypto.randomUUID(),
        roomId: this.#roomId,
        viewerSecret: this.#viewerSecret,
        displayName,
      },
      identity: (nonce) => this.#proveIdentity(nonce),
      onMessage: (message) => void this.#handleMessage(message),
      onStateChange: (signaling) => {
        this.#patch({ signaling });
        this.#diagnostics.record('signaling.state', { state: signaling });
      },
    });
    this.#peer = new ViewerPeerConnection({
      sendSignal: (message) => this.#send(message),
      onStream: (stream) => this.#patch({ stream }),
      onMetrics: (metrics) => this.#patch({ metrics }),
      onState: (connectionState, iceState) => {
        this.#recovery.update(connectionState);
        this.#diagnostics.record('connection.state', { connectionState, iceState });
        this.#patch({
          connectionState,
          iceState,
          phase: connectionState === 'connected' ? 'live' : this.#state.phase,
        });
      },
      onChat: (message) => this.chat.addMessage(message.sender, message.text),
    });
    this.#signaling.connect();
  }

  public disconnect(): void {
    this.#recovery.stop();
    this.#credentialRefresh.stop();
    this.#peer?.close();
    this.#signaling?.disconnect();
    this.#signaling = null;
  }

  public sendChat(text: string): void {
    const trimmed = text.trim();
    if (!trimmed || !this.#peer) return;
    const sender = this.#state.displayName?.trim() || 'Viewer';
    this.chat.addMessage(sender, trimmed, true);
    this.#peer.sendChat({ sender, text: trimmed });
  }

  /** A user-initiated recovery attempt; bypasses the automatic rate limit. */
  public requestReconnect(): void {
    this.#recovery.requestNow();
  }

  public updateDisplayName(displayName: string | null): void {
    if (!this.#signaling || !this.#selfPeerId) return;
    const requestId = crypto.randomUUID();
    this.#identityRequestId = requestId;
    this.#patch({ identityStatus: 'saving', identityError: null });
    try {
      this.#send({
        type: 'viewer:update-display-name',
        protocolVersion: PROTOCOL_VERSION,
        requestId,
        displayName,
      });
    } catch (error) {
      this.#identityRequestId = null;
      this.#patch({
        identityStatus: 'failed',
        identityError: error instanceof Error ? error.message : 'The display name could not be saved.',
      });
    }
  }

  public diagnosticsJson(): string {
    return JSON.stringify(
      this.#diagnostics.export({
        protocolVersion: PROTOCOL_VERSION,
        signaling: this.#state.signaling,
        phase: this.#state.phase,
        connectionState: this.#state.connectionState,
        roomExpiresAt: this.#state.snapshot?.expiresAt,
      }),
      null,
      2,
    );
  }

  async #handleMessage(message: ServerMessage): Promise<void> {
    switch (message.type) {
      case 'auth:succeeded': {
        if (this.#selfPeerId && this.#selfPeerId !== message.peerId) {
          // A fresh authentication replaced an expired resume: the old media
          // connection is orphaned, so drop it and await a fresh offer.
          this.#peer?.close();
          this.#patch({ stream: null, metrics: null });
          this.#diagnostics.record('session.reauthenticated');
        }
        this.#selfPeerId = message.peerId;
        this.#credentialRefresh.schedule(message.iceConfiguration.expiresAt);
        this.#peer?.configure(message.iceConfiguration);
        const self = findSelf(message.snapshot, message.peerId);
        this.#patch({
          snapshot: message.snapshot,
          phase: self?.viewerState === 'approved' ? 'negotiating' : 'awaiting-approval',
          displayName: self?.displayName ?? null,
        });
        break;
      }
      case 'auth:identity-challenge':
        this.#diagnostics.record('auth.identity-challenge');
        break;
      case 'room:snapshot': {
        const self = this.#selfPeerId ? findSelf(message.snapshot, this.#selfPeerId) : undefined;
        this.#patch({
          snapshot: message.snapshot,
          ...(self ? { displayName: self.displayName ?? null } : {}),
        });
        break;
      }
      case 'room:sharing-state-updated':
        if (this.#state.snapshot) {
          if (this.#state.snapshot.sharingState !== message.sharingState) {
            this.chat.addSystem(sharingStateSystemLine(message.sharingState));
          }
          this.#patch({
            snapshot: {
              ...this.#state.snapshot,
              sharingState: message.sharingState,
            },
          });
        }
        break;
      case 'viewer:approved':
        if (message.peerId === this.#selfPeerId) this.#patch({ phase: 'negotiating' });
        break;
      case 'viewer:rejected':
        this.#endSession('rejected');
        break;
      case 'viewer:kicked':
        this.#endSession('kicked');
        break;
      case 'viewer:display-name-updated':
        if (message.peerId === this.#selfPeerId) {
          this.#identityRequestId = null;
          this.#patch({
            displayName: message.displayName ?? null,
            identityStatus: 'saved',
            identityError: null,
          });
        }
        break;
      case 'signal:offer':
        this.#presenterPeerId = message.sourcePeerId;
        this.#patch({ phase: 'negotiating' });
        await this.#peer?.acceptOffer(message.sourcePeerId, message.sdp);
        break;
      case 'signal:ice-candidate':
        await this.#peer?.addRemoteCandidate({
          candidate: message.candidate,
          sdpMid: message.sdpMid,
          sdpMLineIndex: message.sdpMLineIndex,
        });
        break;
      case 'ice:configuration':
        this.#credentialRefresh.schedule(message.configuration.expiresAt);
        this.#peer?.configure(message.configuration);
        break;
      case 'presenter:disconnected':
        this.#patch({ presenterDisconnected: true });
        break;
      case 'presenter:resumed':
        this.#patch({ presenterDisconnected: false });
        break;
      case 'room:closed':
        this.#endSession('room-ended');
        break;
      case 'room:expired':
        this.#endSession('room-expired');
        break;
      case 'error':
        if (message.requestId && message.requestId === this.#identityRequestId) {
          this.#identityRequestId = null;
          this.#patch({ identityStatus: 'failed', identityError: message.message });
          break;
        }
        if (message.requestId && this.#signalRequestIds.delete(message.requestId)) {
          // A routed signal (an ICE restart request) or a credential refresh
          // was rejected, typically because the presenter is disconnected.
          // Recovery retries or the presenter's resume re-offer handle it;
          // the session stays usable.
          this.#diagnostics.record('signal.rejected', { message: message.message });
          break;
        }
        this.#patch({ phase: 'failed', error: message.message });
        break;
      case 'auth:failed':
        this.#patch({
          phase: 'failed',
          error: message.code === 'authentication_failed' && !identityReady()
            ? 'This room is friends-only. Set up your identity from the home screen, then ask the presenter to add your friend code.'
            : message.message,
        });
        break;
      case 'viewer:pending':
      case 'viewer:left':
      case 'viewer:resumed':
      case 'room:capacity-updated':
      case 'signal:answer':
      case 'signal:ice-restart':
        break;
    }
  }

  async #proveIdentity(nonce: Uint8Array): Promise<{ publicKey: string; signature: string } | null> {
    await identityStore.load();
    const identity = identityStore.getSnapshot();
    if (identity.status !== 'ready' || !identity.publicKeyBase64) return null;
    const signature = await identityStore.sign(nonce);
    return { publicKey: identity.publicKeyBase64, signature: bytesToBase64(signature) };
  }

  #requestIceRestart(): void {
    if (!this.#presenterPeerId) return;
    if (this.#state.presenterDisconnected) {
      // The presenter cannot receive the signal; its resume re-offer recovers
      // media instead.
      return;
    }
    this.#diagnostics.record('ice.restart-requested');
    const requestId = crypto.randomUUID();
    this.#signalRequestIds.add(requestId);
    try {
      this.#send({
        type: 'signal:ice-restart',
        protocolVersion: PROTOCOL_VERSION,
        requestId,
        destinationPeerId: this.#presenterPeerId,
      });
    } catch {
      // Signaling is down; its reconnect path re-establishes media instead.
      this.#signalRequestIds.delete(requestId);
    }
  }

  #requestIceRefresh(): void {
    this.#diagnostics.record('ice.refresh-requested');
    const requestId = crypto.randomUUID();
    this.#signalRequestIds.add(requestId);
    try {
      this.#send({
        type: 'ice:refresh',
        protocolVersion: PROTOCOL_VERSION,
        requestId,
      });
    } catch {
      // Signaling is down; re-authentication delivers a fresh configuration.
      this.#signalRequestIds.delete(requestId);
    }
  }

  #endSession(phase: ViewerSessionPhase): void {
    this.#recovery.stop();
    this.#credentialRefresh.stop();
    this.#peer?.close();
    this.#patch({ phase });
  }

  #send(message: ClientMessage): void {
    this.#signaling?.send(message);
  }

  #patch(patch: Partial<ViewerSessionState>): void {
    this.#state = { ...this.#state, ...patch };
    this.#listeners.forEach((listener) => listener());
  }
}

function findSelf(snapshot: RoomSnapshot, peerId: string) {
  return [...snapshot.pendingViewers, ...snapshot.approvedViewers].find(
    (viewer) => viewer.peerId === peerId,
  );
}

function identityReady(): boolean {
  const identity = identityStore.getSnapshot();
  return identity.status === 'ready' && identity.publicKeyBase64 !== null;
}

function sharingStateSystemLine(sharingState: SharingState): string {
  switch (sharingState) {
    case 'live':
      return 'Sharing started';
    case 'paused':
      return 'Sharing paused';
    case 'idle':
      return 'Sharing stopped';
  }
}
