import type { ChatMessage, ClientMessage, IceConfiguration } from '@/generated/protocol';
import { PROTOCOL_VERSION } from '@/config/environment';
import {
  CHAT_CHANNEL_LABEL,
  decodeChatMessage,
  encodeChatMessage,
} from '@/lib/chat/chat-channel';
import { forceRelayEnabled } from '@/lib/settings/app-settings';
import {
  WebRtcStatsCollector,
  type WebRtcMetrics,
} from '@/lib/webrtc/stats-collector';

interface ViewerPeerConnectionOptions {
  sendSignal: (message: ClientMessage) => void;
  onStream: (stream: MediaStream) => void;
  onMetrics: (metrics: WebRtcMetrics) => void;
  onState: (connection: RTCPeerConnectionState, ice: RTCIceConnectionState) => void;
  onChat: (message: ChatMessage) => void;
}

export class ViewerPeerConnection {
  readonly #options: ViewerPeerConnectionOptions;
  #connection: RTCPeerConnection | null = null;
  #iceConfiguration: IceConfiguration | null = null;
  #queuedCandidates: RTCIceCandidateInit[] = [];
  #stats: WebRtcStatsCollector | null = null;
  #stream = new MediaStream();
  #chat: RTCDataChannel | null = null;
  #queuedChat: string[] = [];

  public constructor(options: ViewerPeerConnectionOptions) {
    this.#options = options;
  }

  public configure(configuration: IceConfiguration): void {
    this.#iceConfiguration = configuration;
    if (this.#connection) {
      this.#connection.setConfiguration({
        ...this.#connection.getConfiguration(),
        iceServers: toRtcIceServers(configuration),
      });
    }
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

  /** Sends a chat envelope; queued until the presenter's channel opens. */
  public sendChat(message: ChatMessage): void {
    const payload = encodeChatMessage(message);
    if (this.#chat?.readyState === 'open') {
      this.#chat.send(payload);
    } else {
      this.#queuedChat.push(payload);
    }
  }

  public close(): void {
    this.#stats?.stop();
    this.#stats = null;
    if (this.#chat) {
      this.#chat.onmessage = null;
      this.#chat.onopen = null;
      this.#chat = null;
    }
    this.#connection?.close();
    this.#connection = null;
    this.#queuedCandidates = [];
    this.#stream = new MediaStream();
  }

  #createConnection(presenterPeerId: string): RTCPeerConnection {
    if (!this.#iceConfiguration) throw new Error('ICE configuration is unavailable.');
    const connection = new RTCPeerConnection({
      iceServers: toRtcIceServers(this.#iceConfiguration),
      bundlePolicy: 'max-bundle',
      rtcpMuxPolicy: 'require',
      iceTransportPolicy: forceRelayEnabled() ? 'relay' : 'all',
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
    connection.ontrack = ({ track }) => this.#adoptTrack(track);
    connection.ondatachannel = ({ channel }) => {
      if (channel.label !== CHAT_CHANNEL_LABEL) return;
      this.#adoptChatChannel(channel);
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

  /**
   * Accumulates received tracks into the one locally-owned stream. The native
   * presenter signals msid only at the ssrc level, so browsers can deliver
   * audio and video in different remote streams; adopting `event.streams`
   * would let whichever ontrack fired last win and transiently drop a track.
   * The video element follows track changes on the stream, so mutating this
   * stable object is enough and its identity never changes mid-connection.
   */
  #adoptTrack(track: MediaStreamTrack): void {
    const stream = this.#stream;
    if (stream.getTracks().some((existing) => existing.id === track.id)) return;
    // A renegotiation replaces the sender's track; drop the stale one of the
    // same kind so the element does not keep a dead track around.
    for (const existing of stream.getTracks()) {
      if (existing.kind === track.kind) stream.removeTrack(existing);
    }
    stream.addTrack(track);
    track.addEventListener('ended', () => {
      stream.removeTrack(track);
      // close() swaps in a fresh stream; only report on the live one.
      if (stream === this.#stream) this.#options.onStream(stream);
    });
    this.#options.onStream(stream);
  }

  #adoptChatChannel(channel: RTCDataChannel): void {
    this.#chat = channel;
    channel.onmessage = (event: MessageEvent<unknown>) => {
      const message = decodeChatMessage(event.data);
      if (message) this.#options.onChat(message);
    };
    const flush = () => {
      for (const payload of this.#queuedChat.splice(0)) channel.send(payload);
    };
    if (channel.readyState === 'open') {
      flush();
    } else {
      channel.onopen = flush;
    }
  }
}

function toRtcIceServers(configuration: IceConfiguration): RTCIceServer[] {
  return configuration.iceServers.map((server) => ({
    urls: server.urls,
    ...(server.username ? { username: server.username } : {}),
    ...(server.credential ? { credential: server.credential } : {}),
  }));
}
