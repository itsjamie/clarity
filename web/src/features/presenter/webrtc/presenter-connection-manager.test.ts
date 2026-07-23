import type { IceConfiguration } from '@/generated/protocol';
import { DiagnosticsCollector } from '@/lib/diagnostics/diagnostics-collector';

import { PresenterConnectionManager } from './presenter-connection-manager';

const iceConfiguration: IceConfiguration = {
  expiresAt: '2099-01-01T00:00:00Z',
  iceServers: [],
};

describe('presenter connection manager reconfiguration', () => {
  beforeEach(() => {
    FakePeerConnection.instances.length = 0;
    vi.stubGlobal('RTCPeerConnection', FakePeerConnection);
  });

  afterEach(() => {
    vi.unstubAllGlobals();
  });

  it('reapplies a motion profile when an active text source is replaced', async () => {
    const manager = createManager();
    await manager.configure(iceConfiguration, 'text', 'adaptive', 'auto');
    await manager.setSource(streamWith(videoTrack('text')));
    await manager.addApprovedViewer('viewer-1');
    const connection = activeConnection();

    expect(connection.videoSender.parameters().encodings[0]?.maxFramerate).toBe(30);

    await manager.configure(iceConfiguration, 'motion', 'adaptive', 'auto');
    expect(connection.videoSender.parameters().encodings[0]?.maxFramerate).toBe(30);

    await expect(manager.replaceSource(streamWith(videoTrack('motion')))).resolves.toEqual([]);

    expect(connection.videoSender.parameters().encodings[0]).toMatchObject({
      maxBitrate: 24_000_000,
      maxFramerate: 60,
      scaleResolutionDownBy: 1,
    });
    expect(manager.statuses[0]?.profile.id).toBe('motion-high');
    manager.stopAll();
  });

  it('reapplies the high profile when fixed quality is selected for an active sender', async () => {
    const manager = createManager();
    await manager.configure(iceConfiguration, 'motion', 'adaptive', 'auto');
    await manager.setSource(streamWith(videoTrack('motion')));
    await manager.addApprovedViewer('viewer-1');
    const connection = activeConnection();
    const callsBeforeChange = connection.videoSender.setParameters.mock.calls.length;

    await manager.configure(iceConfiguration, 'motion', 'fixed', 'auto');

    expect(connection.videoSender.setParameters.mock.calls.length).toBeGreaterThan(callsBeforeChange);
    expect(connection.videoSender.parameters().encodings[0]).toMatchObject({
      maxBitrate: 24_000_000,
      maxFramerate: 60,
      scaleResolutionDownBy: 1,
    });
    expect(manager.statuses[0]?.profile.id).toBe('motion-high');
    manager.stopAll();
  });

  it('pauses senders without closing peers and connects late viewers after resume', async () => {
    const manager = createManager();
    const resumedTrack = videoTrack('resumed');
    await manager.configure(iceConfiguration, 'text', 'adaptive', 'auto');
    await manager.setSource(streamWith(videoTrack('initial')));
    await manager.addApprovedViewer('viewer-1');
    const existingConnection = activeConnection();

    await expect(manager.pauseSource()).resolves.toEqual([]);

    expect(existingConnection.videoSender.replaceTrack).toHaveBeenLastCalledWith(null);
    expect(existingConnection.connectionState).not.toBe('closed');
    expect(manager.statuses).toHaveLength(1);

    await manager.addApprovedViewer('viewer-2');
    expect(FakePeerConnection.instances).toHaveLength(1);

    const resumedStream = streamWith(resumedTrack);
    await expect(manager.replaceSource(resumedStream)).resolves.toEqual([]);
    await manager.setSource(resumedStream);

    expect(existingConnection.videoSender.replaceTrack).toHaveBeenLastCalledWith(resumedTrack);
    expect(FakePeerConnection.instances).toHaveLength(2);
    manager.stopAll();
  });
});

function createManager(): PresenterConnectionManager {
  return new PresenterConnectionManager({
    sendSignal: vi.fn(),
    onStatus: vi.fn(),
    diagnostics: new DiagnosticsCollector(),
  });
}

function streamWith(track: MediaStreamTrack): MediaStream {
  return {
    getVideoTracks: () => [track],
    getAudioTracks: () => [],
  } as unknown as MediaStream;
}

function videoTrack(id: string): MediaStreamTrack {
  return { id, kind: 'video' } as MediaStreamTrack;
}

function activeConnection(): FakePeerConnection {
  const connection = FakePeerConnection.instances[0];
  if (!connection) throw new Error('Expected an active peer connection.');
  return connection;
}

class FakeSender {
  readonly setParameters = vi.fn<(parameters: RTCRtpSendParameters) => Promise<void>>(
    (parameters) => {
      this.#parameters = copyParameters(parameters);
      return Promise.resolve();
    },
  );
  readonly replaceTrack = vi.fn<(track: MediaStreamTrack | null) => Promise<void>>(
    () => Promise.resolve(),
  );
  #parameters = { encodings: [{}] } as RTCRtpSendParameters;

  public getParameters(): RTCRtpSendParameters {
    return copyParameters(this.#parameters);
  }

  public parameters(): RTCRtpSendParameters {
    return copyParameters(this.#parameters);
  }
}

class FakePeerConnection {
  static readonly instances: FakePeerConnection[] = [];

  readonly videoSender = new FakeSender();
  connectionState: RTCPeerConnectionState = 'new';
  iceConnectionState: RTCIceConnectionState = 'new';
  signalingState: RTCSignalingState = 'stable';
  remoteDescription: RTCSessionDescription | null = null;
  onicecandidate: ((event: RTCPeerConnectionIceEvent) => void) | null = null;
  onconnectionstatechange: (() => void) | null = null;
  oniceconnectionstatechange: (() => void) | null = null;

  public constructor() {
    FakePeerConnection.instances.push(this);
  }

  public addTransceiver(): RTCRtpTransceiver {
    return { sender: this.videoSender } as unknown as RTCRtpTransceiver;
  }

  public createOffer(): Promise<RTCSessionDescriptionInit> {
    return Promise.resolve({ type: 'offer', sdp: 'v=0' });
  }

  public setLocalDescription(): Promise<void> {
    return Promise.resolve();
  }

  public getStats(): Promise<RTCStatsReport> {
    return Promise.resolve(new Map() as unknown as RTCStatsReport);
  }

  public close(): void {
    this.connectionState = 'closed';
  }
}

function copyParameters(parameters: RTCRtpSendParameters): RTCRtpSendParameters {
  return {
    ...parameters,
    encodings: parameters.encodings.map((encoding) => ({ ...encoding })),
  };
}
