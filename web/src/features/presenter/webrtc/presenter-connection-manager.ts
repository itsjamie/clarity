import type { ChatMessage, ClientMessage, IceConfiguration } from '@/generated/protocol';
import { PROTOCOL_VERSION } from '@/config/environment';
import {
  CHAT_CHANNEL_LABEL,
  decodeChatMessage,
  encodeChatMessage,
  relayChatPayload,
  trySendChatPayload,
} from '@/lib/chat/chat-channel';
import type { DiagnosticsCollector } from '@/lib/diagnostics/diagnostics-collector';
import { forceRelayEnabled } from '@/lib/settings/app-settings';
import {
  CodecCapabilityService,
  type CodecMode,
} from '@/lib/webrtc/codec-capability-service';
import {
  QualityAdaptationController,
  type AdaptationDecision,
} from '@/lib/webrtc/quality-adaptation';
import type {
  CaptureMode,
  EncodingProfile,
  QualityStrategy,
} from '@/lib/webrtc/profiles';
import { SenderParameterController } from '@/lib/webrtc/sender-parameter-controller';
import {
  WebRtcStatsCollector,
  type WebRtcMetrics,
} from '@/lib/webrtc/stats-collector';

export interface PresenterPeerStatus {
  peerId: string;
  connectionState: RTCPeerConnectionState;
  iceState: RTCIceConnectionState;
  signalingState: RTCSignalingState;
  profile: EncodingProfile;
  metrics?: WebRtcMetrics;
  lastAdaptationReason: string;
  iceRestartCount: number;
}

interface PresenterConnectionManagerOptions {
  sendSignal: (message: ClientMessage) => void;
  onStatus: (status: PresenterPeerStatus) => void;
  onChat: (peerId: string, message: ChatMessage) => void;
  diagnostics: DiagnosticsCollector;
}

interface PeerEntry {
  peerId: string;
  connection: RTCPeerConnection;
  videoSender: RTCRtpSender;
  audioSender: RTCRtpSender | null;
  chat: RTCDataChannel | null;
  negotiatedWithTrack: boolean;
  mode: CaptureMode;
  queuedCandidates: RTCIceCandidateInit[];
  stats: WebRtcStatsCollector;
  adaptation: QualityAdaptationController;
  profile: EncodingProfile;
  lastAdaptationReason: string;
  iceRestartCount: number;
  recoveryTimer: number | null;
  metrics?: WebRtcMetrics;
}

interface SourceReplacementSnapshot {
  entry: PeerEntry;
  videoTrack: MediaStreamTrack | null;
  audioSender: RTCRtpSender | null;
  audioTrack: MediaStreamTrack | null;
  negotiatedWithTrack: boolean;
  mode: CaptureMode;
  adaptation: QualityAdaptationController;
  profile: EncodingProfile;
  lastAdaptationReason: string;
  videoParameters: RTCRtpSendParameters;
}

export class PresenterConnectionManager {
  readonly #options: PresenterConnectionManagerOptions;
  readonly #entries = new Map<string, PeerEntry>();
  readonly #creating = new Map<string, Promise<void>>();
  readonly #approvedViewerIds = new Set<string>();
  readonly #displayNames = new Map<string, string | null>();
  readonly #senderParameters = new SenderParameterController();
  readonly #codecs = new CodecCapabilityService();
  #source: MediaStream | null = null;
  #iceConfiguration: IceConfiguration | null = null;
  #mode: CaptureMode = 'text';
  #qualityStrategy: QualityStrategy = 'adaptive';
  #codecMode: CodecMode = 'auto';

  public constructor(options: PresenterConnectionManagerOptions) {
    this.#options = options;
  }

  public get statuses(): PresenterPeerStatus[] {
    return [...this.#entries.values()].map((entry) => this.#status(entry));
  }

  public async configure(
    iceConfiguration: IceConfiguration,
    mode: CaptureMode,
    strategy: QualityStrategy,
    codec: CodecMode,
  ): Promise<void> {
    const strategyChanged = this.#qualityStrategy !== strategy;
    this.#iceConfiguration = iceConfiguration;
    this.#mode = mode;
    this.#qualityStrategy = strategy;
    this.#codecMode = codec;
    if (!strategyChanged) return;
    await Promise.all(
      [...this.#entries.values()].map((entry) => this.#applyStrategy(entry, strategy)),
    );
  }

  public async setSource(stream: MediaStream): Promise<string[]> {
    let failures: string[] = [];
    if (this.#entries.size > 0) {
      failures = await this.replaceSource(stream);
    } else {
      this.#source = stream;
    }
    for (const peerId of this.#approvedViewerIds) {
      await this.#ensurePeer(peerId);
    }
    return failures;
  }

  public async addApprovedViewer(peerId: string): Promise<void> {
    this.#approvedViewerIds.add(peerId);
    await this.#ensurePeer(peerId);
  }

  /**
   * Records the server-known display name chat from this viewer is stamped
   * with when relayed; `null` falls back to "Viewer".
   */
  public setViewerDisplayName(peerId: string, displayName: string | null): void {
    this.#displayNames.set(peerId, displayName);
  }

  public removeViewer(peerId: string): void {
    this.#approvedViewerIds.delete(peerId);
    this.#displayNames.delete(peerId);
    const entry = this.#entries.get(peerId);
    if (!entry) return;
    this.#discardEntry(entry);
  }

  #discardEntry(entry: PeerEntry): void {
    if (entry.recoveryTimer !== null) window.clearTimeout(entry.recoveryTimer);
    entry.stats.stop();
    if (entry.chat) {
      entry.chat.onmessage = null;
      entry.chat = null;
    }
    entry.connection.onicecandidate = null;
    entry.connection.onconnectionstatechange = null;
    entry.connection.close();
    entry.queuedCandidates.length = 0;
    if (this.#entries.get(entry.peerId) === entry) this.#entries.delete(entry.peerId);
    this.#options.diagnostics.record('peer.removed', { peerId: entry.peerId });
  }

  /** Sends a chat envelope to every viewer whose channel is open. */
  public sendChat(message: ChatMessage): void {
    const payload = encodeChatMessage(message);
    for (const entry of this.#entries.values()) {
      trySendChatPayload(entry.chat, payload);
    }
  }

  public async applyAnswer(peerId: string, sdp: string): Promise<void> {
    const entry = this.#entries.get(peerId);
    if (!entry) return;
    await entry.connection.setRemoteDescription({ type: 'answer', sdp });
    for (const candidate of entry.queuedCandidates.splice(0)) {
      await entry.connection.addIceCandidate(candidate);
    }
  }

  public async addRemoteCandidate(peerId: string, candidate: RTCIceCandidateInit): Promise<void> {
    const entry = this.#entries.get(peerId);
    if (!entry) return;
    if (!entry.connection.remoteDescription) {
      entry.queuedCandidates.push(candidate);
    } else {
      await entry.connection.addIceCandidate(candidate);
    }
  }

  public async restartIce(peerId: string): Promise<void> {
    const entry = this.#entries.get(peerId);
    if (!entry || entry.connection.signalingState === 'closed') return;
    entry.iceRestartCount += 1;
    entry.connection.restartIce();
    await this.#negotiate(entry, true);
    this.#emit(entry);
  }

  public async replaceSource(stream: MediaStream): Promise<string[]> {
    const videoTrack = stream.getVideoTracks()[0];
    if (!videoTrack) throw new Error('The replacement source has no video track.');
    const audioTrack = stream.getAudioTracks()[0] ?? null;
    const failures = new Set<string>();
    const replacements: SourceReplacementSnapshot[] = [];
    for (const entry of this.#entries.values()) {
      const snapshot = this.#sourceSnapshot(entry);
      replacements.push(snapshot);
      try {
        let needsOffer = !entry.negotiatedWithTrack;
        await entry.videoSender.replaceTrack(videoTrack);
        if (entry.audioSender) {
          await entry.audioSender.replaceTrack(audioTrack);
        } else if (audioTrack) {
          entry.audioSender = entry.connection.addTrack(audioTrack, stream);
          needsOffer = true;
        }
        if (needsOffer) {
          // A peer born in an idle room negotiated its media section without
          // a track; re-offer so the session is negotiated against the real
          // source and its stream ids, like a peer created mid-share.
          await this.#negotiate(entry, false);
          entry.negotiatedWithTrack = true;
        }
        if (entry.mode === this.#mode) {
          entry.adaptation.reset();
        } else {
          entry.mode = this.#mode;
          entry.adaptation = new QualityAdaptationController(
            entry.mode,
            this.#qualityStrategy,
          );
          entry.profile = entry.adaptation.profile;
          entry.lastAdaptationReason = `Capture mode changed. Reset to ${entry.profile.label}.`;
          const result = await this.#senderParameters.apply(
            entry.videoSender,
            entry.profile,
            entry.mode,
          );
          this.#options.diagnostics.record('quality.mode-changed', {
            peerId: entry.peerId,
            profile: entry.profile.id,
            unsupportedParameters: result.unsupported,
          });
        }
        this.#emit(entry);
      } catch {
        failures.add(entry.peerId);
        break;
      }
    }
    if (failures.size === 0) {
      this.#source = stream;
      return [];
    }
    for (const snapshot of replacements.reverse()) {
      try {
        await this.#rollbackSource(snapshot);
      } catch {
        failures.add(snapshot.entry.peerId);
        this.#options.diagnostics.record('source.rollback-failed', {
          peerId: snapshot.entry.peerId,
        });
        // Never leave a live entry pointing at the replacement stream that the
        // capture manager is about to stop. Rebuild it from the old source.
        this.#discardEntry(snapshot.entry);
        if (
          this.#approvedViewerIds.has(snapshot.entry.peerId) &&
          !this.#entries.has(snapshot.entry.peerId)
        ) {
          try {
            await this.#createPeer(snapshot.entry.peerId);
          } catch {
            this.#options.diagnostics.record('peer.rebuild-failed', {
              peerId: snapshot.entry.peerId,
            });
          }
        }
      }
    }
    return [...failures];
  }

  #sourceSnapshot(entry: PeerEntry): SourceReplacementSnapshot {
    return {
      entry,
      videoTrack: entry.videoSender.track,
      audioSender: entry.audioSender,
      audioTrack: entry.audioSender?.track ?? null,
      negotiatedWithTrack: entry.negotiatedWithTrack,
      mode: entry.mode,
      adaptation: entry.adaptation.clone(),
      profile: entry.profile,
      lastAdaptationReason: entry.lastAdaptationReason,
      videoParameters: entry.videoSender.getParameters(),
    };
  }

  async #rollbackSource(snapshot: SourceReplacementSnapshot): Promise<void> {
    const { entry } = snapshot;
    if (this.#entries.get(entry.peerId) !== entry) return;
    await entry.videoSender.replaceTrack(snapshot.videoTrack);
    const audioSenderChanged = entry.audioSender !== snapshot.audioSender;
    if (audioSenderChanged) {
      if (entry.audioSender) entry.connection.removeTrack(entry.audioSender);
      entry.audioSender = snapshot.audioSender;
    }
    await snapshot.audioSender?.replaceTrack(snapshot.audioTrack);
    const negotiationChanged =
      audioSenderChanged || entry.negotiatedWithTrack !== snapshot.negotiatedWithTrack;
    entry.negotiatedWithTrack = snapshot.negotiatedWithTrack;
    entry.mode = snapshot.mode;
    entry.adaptation = snapshot.adaptation;
    entry.profile = snapshot.profile;
    entry.lastAdaptationReason = snapshot.lastAdaptationReason;
    await entry.videoSender.setParameters(snapshot.videoParameters);
    if (negotiationChanged) await this.#negotiate(entry, false);
    this.#emit(entry);
  }

  public async pauseSource(): Promise<string[]> {
    const failures: string[] = [];
    for (const entry of this.#entries.values()) {
      try {
        await entry.videoSender.replaceTrack(null);
        await entry.audioSender?.replaceTrack(null);
        this.#emit(entry);
      } catch {
        failures.push(entry.peerId);
      }
    }
    this.#source = null;
    return failures;
  }

  public stopAll(): void {
    for (const peerId of [...this.#entries.keys()]) this.removeViewer(peerId);
    this.#approvedViewerIds.clear();
    this.#source = null;
  }

  async #createPeer(peerId: string): Promise<void> {
    if (!this.#iceConfiguration || !this.#approvedViewerIds.has(peerId)) return;
    const connection = new RTCPeerConnection({
      iceServers: toRtcIceServers(this.#iceConfiguration),
      bundlePolicy: 'max-bundle',
      rtcpMuxPolicy: 'require',
      iceTransportPolicy: forceRelayEnabled() ? 'relay' : 'all',
    });
    // The chat channel is created before negotiation so it rides the first
    // offer; its label and JSON envelope match the native engine.
    const videoTrack = this.#source?.getVideoTracks()[0] ?? null;
    // Without a source (idle room) a track-less sendonly transceiver keeps
    // the media section ready, so starting a share never renegotiates.
    let chat: RTCDataChannel;
    let transceiver: RTCRtpTransceiver;
    try {
      chat = connection.createDataChannel(CHAT_CHANNEL_LABEL);
      chat.onmessage = (event: MessageEvent<unknown>) => this.#onChatPayload(peerId, event.data);
      transceiver = videoTrack && this.#source
        ? connection.addTransceiver(videoTrack, { direction: 'sendonly', streams: [this.#source] })
        : connection.addTransceiver('video', { direction: 'sendonly' });
    } catch (error) {
      connection.close();
      throw error;
    }
    try {
      await this.#codecs.applyPreference(transceiver, this.#codecMode);
    } catch (error) {
      connection.close();
      throw error;
    }
    if (!this.#approvedViewerIds.has(peerId)) {
      connection.close();
      return;
    }
    const audioTrack = this.#source?.getAudioTracks()[0];
    let audioSender: RTCRtpSender | null;
    try {
      audioSender = audioTrack && this.#source
        ? connection.addTrack(audioTrack, this.#source)
        : null;
    } catch (error) {
      connection.close();
      throw error;
    }
    const adaptation = new QualityAdaptationController(this.#mode, this.#qualityStrategy);
    const entry: PeerEntry = {
      peerId,
      connection,
      videoSender: transceiver.sender,
      audioSender,
      chat,
      negotiatedWithTrack: videoTrack !== null,
      mode: this.#mode,
      queuedCandidates: [],
      stats: new WebRtcStatsCollector(connection, 'outbound', (metrics) =>
        void this.#onMetrics(peerId, metrics),
      ),
      adaptation,
      profile: adaptation.profile,
      lastAdaptationReason: 'Initial profile selected.',
      iceRestartCount: 0,
      recoveryTimer: null,
    };
    this.#entries.set(peerId, entry);
    connection.onicecandidate = ({ candidate }) => {
      if (!candidate) return;
      const value = candidate.toJSON();
      this.#options.sendSignal({
        type: 'signal:ice-candidate',
        protocolVersion: PROTOCOL_VERSION,
        requestId: crypto.randomUUID(),
        destinationPeerId: peerId,
        candidate: value.candidate ?? '',
        sdpMid: value.sdpMid ?? null,
        sdpMLineIndex: value.sdpMLineIndex ?? null,
      });
    };
    connection.onconnectionstatechange = () => {
      this.#emit(entry);
      if (connection.connectionState === 'failed') void this.#recover(entry);
    };
    connection.oniceconnectionstatechange = () => this.#emit(entry);
    try {
      await this.#senderParameters.apply(entry.videoSender, entry.profile, entry.mode);
      if (!this.#entryIsActive(entry)) return;
      await this.#negotiate(entry, false);
      if (!this.#entryIsActive(entry)) return;
    } catch (error) {
      if (this.#entries.get(peerId) === entry) {
        entry.connection.close();
        this.#entries.delete(peerId);
        throw error;
      }
      // Removal while an awaited browser operation was in flight is an
      // intentional cancellation, not a failed peer creation.
      return;
    }
    entry.stats.start();
    this.#options.diagnostics.record('peer.created', { peerId });
    this.#emit(entry);
  }

  async #ensurePeer(peerId: string): Promise<void> {
    if (!this.#approvedViewerIds.has(peerId) || !this.#iceConfiguration) return;
    const existing = this.#creating.get(peerId);
    if (existing) {
      await existing;
      if (this.#approvedViewerIds.has(peerId) && !this.#entries.has(peerId)) {
        await this.#ensurePeer(peerId);
      }
      return;
    }
    if (this.#entries.has(peerId)) return;
    const creation = this.#createPeer(peerId).finally(() => {
      if (this.#creating.get(peerId) === creation) this.#creating.delete(peerId);
    });
    this.#creating.set(peerId, creation);
    await creation;
    if (this.#approvedViewerIds.has(peerId) && !this.#entries.has(peerId)) {
      await this.#ensurePeer(peerId);
    }
  }

  #entryIsActive(entry: PeerEntry): boolean {
    return this.#approvedViewerIds.has(entry.peerId) && this.#entries.get(entry.peerId) === entry;
  }

  async #negotiate(entry: PeerEntry, iceRestart: boolean): Promise<void> {
    const offer = await entry.connection.createOffer({ iceRestart });
    await entry.connection.setLocalDescription(offer);
    this.#options.sendSignal({
      type: 'signal:offer',
      protocolVersion: PROTOCOL_VERSION,
      requestId: crypto.randomUUID(),
      destinationPeerId: entry.peerId,
      sdp: offer.sdp ?? '',
      iceRestart,
    });
  }

  async #onMetrics(peerId: string, metrics: WebRtcMetrics): Promise<void> {
    const entry = this.#entries.get(peerId);
    if (!entry) return;
    const decision: AdaptationDecision = entry.adaptation.evaluate(metrics, performance.now());
    entry.metrics = metrics;
    entry.profile = decision.profile;
    entry.lastAdaptationReason = decision.reason;
    if (decision.changed) {
      const result = await this.#senderParameters.apply(
        entry.videoSender,
        decision.profile,
        entry.mode,
      );
      this.#options.diagnostics.record('quality.changed', {
        peerId,
        profile: decision.profile.id,
        reason: decision.reason,
        unsupportedParameters: result.unsupported,
      });
    }
    this.#emit(entry, metrics);
  }

  async #applyStrategy(entry: PeerEntry, strategy: QualityStrategy): Promise<void> {
    entry.adaptation.setStrategy(strategy);
    entry.profile = strategy === 'fixed'
      ? entry.adaptation.setFixedProfile(0)
      : entry.adaptation.profile;
    entry.lastAdaptationReason = strategy === 'fixed'
      ? `Quality locked to ${entry.profile.label}.`
      : 'Adaptive quality enabled.';
    const result = await this.#senderParameters.apply(
      entry.videoSender,
      entry.profile,
      entry.mode,
    );
    this.#options.diagnostics.record('quality.strategy-changed', {
      peerId: entry.peerId,
      profile: entry.profile.id,
      strategy,
      unsupportedParameters: result.unsupported,
    });
    this.#emit(entry);
  }

  async #recover(entry: PeerEntry): Promise<void> {
    if (entry.recoveryTimer !== null) return;
    await this.restartIce(entry.peerId);
    entry.recoveryTimer = window.setTimeout(() => {
      entry.recoveryTimer = null;
      if (entry.connection.connectionState === 'failed') {
        const peerId = entry.peerId;
        this.removeViewer(peerId);
        this.#approvedViewerIds.add(peerId);
        void this.#ensurePeer(peerId);
      }
    }, 8_000);
  }

  #onChatPayload(fromPeerId: string, payload: unknown): void {
    const message = decodeChatMessage(payload);
    if (!message) {
      this.#options.diagnostics.record('chat.dropped', { peerId: fromPeerId });
      return;
    }
    // The envelope's sender field is client-asserted, so the relay hub stamps
    // it with the server-known display name of the peer the payload arrived
    // from; a viewer cannot speak as the presenter or another viewer.
    const stamped: ChatMessage = {
      sender: this.#displayNames.get(fromPeerId) ?? 'Viewer',
      text: message.text,
    };
    relayChatPayload(
      [...this.#entries.values()].map((entry) => [entry.peerId, entry.chat] as const),
      fromPeerId,
      encodeChatMessage(stamped),
    );
    this.#options.onChat(fromPeerId, stamped);
  }

  #emit(entry: PeerEntry, metrics?: WebRtcMetrics): void {
    if (metrics) entry.metrics = metrics;
    this.#options.onStatus(this.#status(entry));
  }

  #status(entry: PeerEntry): PresenterPeerStatus {
    return {
      peerId: entry.peerId,
      connectionState: entry.connection.connectionState,
      iceState: entry.connection.iceConnectionState,
      signalingState: entry.connection.signalingState,
      profile: entry.profile,
      lastAdaptationReason: entry.lastAdaptationReason,
      iceRestartCount: entry.iceRestartCount,
      metrics: entry.metrics,
    };
  }
}

function toRtcIceServers(configuration: IceConfiguration): RTCIceServer[] {
  return configuration.iceServers.map((server) => ({
    urls: server.urls,
    ...(server.username ? { username: server.username } : {}),
    ...(server.credential ? { credential: server.credential } : {}),
  }));
}
