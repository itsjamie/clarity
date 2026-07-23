import type { ClientMessage, IceConfiguration } from '@/generated/protocol';
import { PROTOCOL_VERSION } from '@/config/environment';
import {
  WebRtcStatsCollector,
  type WebRtcMetrics,
} from '@/lib/webrtc/stats-collector';

interface ViewerPeerConnectionOptions {
  sendSignal: (message: ClientMessage) => void;
  onStream: (stream: MediaStream) => void;
  onMetrics: (metrics: WebRtcMetrics) => void;
  onState: (connection: RTCPeerConnectionState, ice: RTCIceConnectionState) => void;
}

export class ViewerPeerConnection {
  readonly #options: ViewerPeerConnectionOptions;
  #connection: RTCPeerConnection | null = null;
  #iceConfiguration: IceConfiguration | null = null;
  #queuedCandidates: RTCIceCandidateInit[] = [];
  #stats: WebRtcStatsCollector | null = null;
  #stream = new MediaStream();

  public constructor(options: ViewerPeerConnectionOptions) {
    this.#options = options;
  }

  public configure(configuration: IceConfiguration): void {
    this.#iceConfiguration = configuration;
  }

  public async acceptOffer(presenterPeerId: string, sdp: string): Promise<void> {
    const connection = this.#connection ?? this.#createConnection(presenterPeerId);
    await connection.setRemoteDescription({ type: 'offer', sdp });
    for (const candidate of this.#queuedCandidates.splice(0)) {
      await connection.addIceCandidate(candidate);
    }
    const answer = await connection.createAnswer();
    await connection.setLocalDescription(answer);
    this.#options.sendSignal({
      type: 'signal:answer',
      protocolVersion: PROTOCOL_VERSION,
      requestId: crypto.randomUUID(),
      destinationPeerId: presenterPeerId,
      sdp: answer.sdp ?? '',
    });
  }

  public async addRemoteCandidate(candidate: RTCIceCandidateInit): Promise<void> {
    if (!this.#connection?.remoteDescription) {
      this.#queuedCandidates.push(candidate);
    } else {
      await this.#connection.addIceCandidate(candidate);
    }
  }

  public close(): void {
    this.#stats?.stop();
    this.#stats = null;
    this.#connection?.close();
    this.#connection = null;
    this.#queuedCandidates = [];
    this.#stream = new MediaStream();
  }

  #createConnection(presenterPeerId: string): RTCPeerConnection {
    if (!this.#iceConfiguration) throw new Error('ICE configuration is unavailable.');
    const connection = new RTCPeerConnection({
      iceServers: this.#iceConfiguration.iceServers.map((server) => ({
        urls: server.urls,
        ...(server.username ? { username: server.username } : {}),
        ...(server.credential ? { credential: server.credential } : {}),
      })),
      bundlePolicy: 'max-bundle',
      rtcpMuxPolicy: 'require',
      iceTransportPolicy:
        import.meta.env.MODE === 'test' &&
        window.sessionStorage.getItem('clarity:test:force-relay') === 'enabled'
          ? 'relay'
          : 'all',
    });
    connection.onicecandidate = ({ candidate }) => {
      if (!candidate) return;
      const value = candidate.toJSON();
      this.#options.sendSignal({
        type: 'signal:ice-candidate',
        protocolVersion: PROTOCOL_VERSION,
        requestId: crypto.randomUUID(),
        destinationPeerId: presenterPeerId,
        candidate: value.candidate ?? '',
        sdpMid: value.sdpMid ?? null,
        sdpMLineIndex: value.sdpMLineIndex ?? null,
      });
    };
    connection.ontrack = ({ track, streams }) => {
      const stream = streams[0];
      if (stream) {
        this.#stream = stream;
      } else if (!this.#stream.getTracks().some((existing) => existing.id === track.id)) {
        this.#stream.addTrack(track);
      }
      this.#options.onStream(this.#stream);
    };
    const emitState = () =>
      this.#options.onState(connection.connectionState, connection.iceConnectionState);
    connection.onconnectionstatechange = emitState;
    connection.oniceconnectionstatechange = emitState;
    this.#stats = new WebRtcStatsCollector(connection, 'inbound', this.#options.onMetrics);
    this.#stats.start();
    this.#connection = connection;
    return connection;
  }
}
