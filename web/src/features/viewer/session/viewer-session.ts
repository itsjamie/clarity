import type { ClientMessage, RoomSnapshot, ServerMessage } from '@/generated/protocol';
import { PROTOCOL_VERSION, signalingUrl } from '@/config/environment';
import type { ExternalStateStore } from '@/hooks/use-session-state';
import {
  SignalingClient,
  type SignalingState,
} from '@/lib/signaling/signaling-client';
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
  readonly #listeners = new Set<() => void>();
  readonly #roomId: string;
  readonly #viewerSecret: string;
  #signaling: SignalingClient | null = null;
  #peer: ViewerPeerConnection | null = null;
  #selfPeerId: string | null = null;
  #identityRequestId: string | null = null;
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
      onMessage: (message) => void this.#handleMessage(message),
      onStateChange: (signaling) => this.#patch({ signaling }),
    });
    this.#peer = new ViewerPeerConnection({
      sendSignal: (message) => this.#send(message),
      onStream: (stream) => this.#patch({ stream }),
      onMetrics: (metrics) => this.#patch({ metrics }),
      onState: (connectionState, iceState) => {
        this.#patch({
          connectionState,
          iceState,
          phase: connectionState === 'connected' ? 'live' : this.#state.phase,
        });
      },
    });
    this.#signaling.connect();
  }

  public disconnect(): void {
    this.#peer?.close();
    this.#signaling?.disconnect();
    this.#signaling = null;
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

  async #handleMessage(message: ServerMessage): Promise<void> {
    switch (message.type) {
      case 'auth:succeeded': {
        this.#selfPeerId = message.peerId;
        this.#peer?.configure(message.iceConfiguration);
        const self = findSelf(message.snapshot, message.peerId);
        this.#patch({
          snapshot: message.snapshot,
          phase: self?.viewerState === 'approved' ? 'negotiating' : 'awaiting-approval',
          displayName: self?.displayName ?? null,
        });
        break;
      }
      case 'room:snapshot': {
        const self = this.#selfPeerId ? findSelf(message.snapshot, this.#selfPeerId) : undefined;
        this.#patch({
          snapshot: message.snapshot,
          ...(self ? { displayName: self.displayName ?? null } : {}),
        });
        break;
      }
      case 'viewer:approved':
        if (message.peerId === this.#selfPeerId) this.#patch({ phase: 'negotiating' });
        break;
      case 'viewer:rejected':
        this.#peer?.close();
        this.#patch({ phase: 'rejected' });
        break;
      case 'viewer:kicked':
        this.#peer?.close();
        this.#patch({ phase: 'kicked' });
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
        this.#peer?.configure(message.configuration);
        break;
      case 'presenter:disconnected':
        this.#patch({ presenterDisconnected: true });
        break;
      case 'presenter:resumed':
        this.#patch({ presenterDisconnected: false });
        break;
      case 'room:closed':
        this.#peer?.close();
        this.#patch({ phase: 'room-ended' });
        break;
      case 'room:expired':
        this.#peer?.close();
        this.#patch({ phase: 'room-expired' });
        break;
      case 'error':
        if (message.requestId && message.requestId === this.#identityRequestId) {
          this.#identityRequestId = null;
          this.#patch({ identityStatus: 'failed', identityError: message.message });
          break;
        }
        this.#patch({ phase: 'failed', error: message.message });
        break;
      case 'auth:failed':
        this.#patch({ phase: 'failed', error: message.message });
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
