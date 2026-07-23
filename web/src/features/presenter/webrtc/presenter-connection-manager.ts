import type { ClientMessage, IceConfiguration } from '@/generated/protocol';
import { PROTOCOL_VERSION } from '@/config/environment';
import type { DiagnosticsCollector } from '@/lib/diagnostics/diagnostics-collector';
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
  diagnostics: DiagnosticsCollector;
}

interface PeerEntry {
  peerId: string;
  connection: RTCPeerConnection;
  videoSender: RTCRtpSender;
  audioSender: RTCRtpSender | null;
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

export class PresenterConnectionManager {
  readonly #options: PresenterConnectionManagerOptions;
  readonly #entries = new Map<string, PeerEntry>();
  readonly #approvedViewerIds = new Set<string>();
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

  public async setSource(stream: MediaStream): Promise<void> {
    this.#source = stream;
    for (const peerId of this.#approvedViewerIds) {
      if (!this.#entries.has(peerId)) await this.#createPeer(peerId);
    }
  }

  public async addApprovedViewer(peerId: string): Promise<void> {
    this.#approvedViewerIds.add(peerId);
    if (this.#source && !this.#entries.has(peerId)) await this.#createPeer(peerId);
  }

  public removeViewer(peerId: string): void {
    this.#approvedViewerIds.delete(peerId);
    const entry = this.#entries.get(peerId);
    if (!entry) return;
    if (entry.recoveryTimer !== null) window.clearTimeout(entry.recoveryTimer);
    entry.stats.stop();
    entry.connection.onicecandidate = null;
    entry.connection.onconnectionstatechange = null;
    entry.connection.close();
    entry.queuedCandidates.length = 0;
    this.#entries.delete(peerId);
    this.#options.diagnostics.record('peer.removed', { peerId });
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
    const failures: string[] = [];
    for (const entry of this.#entries.values()) {
      try {
        await entry.videoSender.replaceTrack(videoTrack);
        if (entry.audioSender) {
          await entry.audioSender.replaceTrack(audioTrack);
        } else if (audioTrack) {
          entry.audioSender = entry.connection.addTrack(audioTrack, stream);
          await this.#negotiate(entry, false);
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
        failures.push(entry.peerId);
      }
    }
    if (failures.length === 0) this.#source = stream;
    return failures;
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
    if (!this.#source || !this.#iceConfiguration) return;
    const connection = new RTCPeerConnection({
      iceServers: toRtcIceServers(this.#iceConfiguration),
      bundlePolicy: 'max-bundle',
      rtcpMuxPolicy: 'require',
      iceTransportPolicy: forcedRelayEnabled() ? 'relay' : 'all',
    });
    const videoTrack = this.#source.getVideoTracks()[0];
    if (!videoTrack) throw new Error('The active source has no video track.');
    const transceiver = connection.addTransceiver(videoTrack, {
      direction: 'sendonly',
      streams: [this.#source],
    });
    await this.#codecs.applyPreference(transceiver, this.#codecMode);
    const audioTrack = this.#source.getAudioTracks()[0];
    const audioSender = audioTrack ? connection.addTrack(audioTrack, this.#source) : null;
    const adaptation = new QualityAdaptationController(this.#mode, this.#qualityStrategy);
    const entry: PeerEntry = {
      peerId,
      connection,
      videoSender: transceiver.sender,
      audioSender,
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
    await this.#senderParameters.apply(entry.videoSender, entry.profile, entry.mode);
    await this.#negotiate(entry, false);
    entry.stats.start();
    this.#options.diagnostics.record('peer.created', { peerId });
    this.#emit(entry);
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
        void this.#createPeer(peerId);
      }
    }, 8_000);
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

function forcedRelayEnabled(): boolean {
  return (
    import.meta.env.MODE === 'test' &&
    window.sessionStorage.getItem('clarity:test:force-relay') === 'enabled'
  );
}
