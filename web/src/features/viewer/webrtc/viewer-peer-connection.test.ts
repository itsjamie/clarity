import type { IceConfiguration } from '@/generated/protocol';

import { ViewerPeerConnection } from './viewer-peer-connection';

const iceConfiguration: IceConfiguration = {
  expiresAt: '2099-01-01T00:00:00Z',
  iceServers: [],
};

describe('viewer peer connection track adoption', () => {
  beforeEach(() => {
    FakePeerConnection.instances.length = 0;
    vi.stubGlobal('MediaStream', FakeMediaStream);
    vi.stubGlobal('RTCPeerConnection', FakePeerConnection);
  });

  afterEach(() => {
    vi.unstubAllGlobals();
  });

  it('merges tracks delivered in separate remote streams into one stream', async () => {
    const { viewer, onStream, connection } = await connectedViewer();
    const audio = fakeTrack('audio-1', 'audio');
    const video = fakeTrack('video-1', 'video');

    connection.fireTrack(audio, [new FakeMediaStream()]);
    connection.fireTrack(video, [new FakeMediaStream()]);

    expect(onStream).toHaveBeenCalledTimes(2);
    const stream = onStream.mock.calls[1]?.[0] as unknown as FakeMediaStream;
    expect(onStream.mock.calls[0]?.[0]).toBe(stream);
    expect(trackIds(stream)).toEqual(['audio-1', 'video-1']);
    viewer.close();
  });

  it('ignores a re-fired ontrack for a track it already holds', async () => {
    const { viewer, onStream, connection } = await connectedViewer();
    const video = fakeTrack('video-1', 'video');

    connection.fireTrack(video, []);
    connection.fireTrack(video, [new FakeMediaStream()]);

    expect(onStream).toHaveBeenCalledTimes(1);
    expect(trackIds(onStream.mock.calls[0]?.[0] as unknown as FakeMediaStream)).toEqual([
      'video-1',
    ]);
    viewer.close();
  });

  it('replaces a same-kind track when a renegotiation swaps the sender', async () => {
    const { viewer, onStream, connection } = await connectedViewer();
    connection.fireTrack(fakeTrack('audio-1', 'audio'), []);
    connection.fireTrack(fakeTrack('video-1', 'video'), []);

    connection.fireTrack(fakeTrack('video-2', 'video'), []);

    const stream = onStream.mock.lastCall?.[0] as unknown as FakeMediaStream;
    expect(trackIds(stream)).toEqual(['audio-1', 'video-2']);
    viewer.close();
  });

  it('drops an ended track and reports the mutation', async () => {
    const { viewer, onStream, connection } = await connectedViewer();
    const audio = fakeTrack('audio-1', 'audio');
    const video = fakeTrack('video-1', 'video');
    connection.fireTrack(audio, []);
    connection.fireTrack(video, []);

    audio.end();

    expect(onStream).toHaveBeenCalledTimes(3);
    const stream = onStream.mock.lastCall?.[0] as unknown as FakeMediaStream;
    expect(trackIds(stream)).toEqual(['video-1']);
    viewer.close();
  });

  it('stays quiet when a track ends after close', async () => {
    const { viewer, onStream, connection } = await connectedViewer();
    const video = fakeTrack('video-1', 'video');
    connection.fireTrack(video, []);
    viewer.close();
    const callsAtClose = onStream.mock.calls.length;

    video.end();

    expect(onStream).toHaveBeenCalledTimes(callsAtClose);
  });
});

async function connectedViewer(): Promise<{
  viewer: ViewerPeerConnection;
  onStream: ReturnType<typeof vi.fn>;
  connection: FakePeerConnection;
}> {
  const onStream = vi.fn();
  const viewer = new ViewerPeerConnection({
    sendSignal: vi.fn(),
    onStream,
    onMetrics: vi.fn(),
    onState: vi.fn(),
    onChat: vi.fn(),
  });
  viewer.configure(iceConfiguration);
  await viewer.acceptOffer('presenter-1', 'v=0');
  const connection = FakePeerConnection.instances[0];
  if (!connection) throw new Error('Expected an active peer connection.');
  return { viewer, onStream, connection };
}

function trackIds(stream: FakeMediaStream): string[] {
  return stream.getTracks().map((track) => track.id);
}

function fakeTrack(id: string, kind: 'audio' | 'video'): FakeTrack {
  return new FakeTrack(id, kind);
}

class FakeTrack {
  readonly #endedListeners: Array<() => void> = [];

  public constructor(
    public readonly id: string,
    public readonly kind: 'audio' | 'video',
  ) {}

  public addEventListener(type: string, listener: () => void): void {
    if (type === 'ended') this.#endedListeners.push(listener);
  }

  public end(): void {
    for (const listener of this.#endedListeners) listener();
  }
}

class FakeMediaStream {
  readonly #tracks: FakeTrack[] = [];

  public getTracks(): FakeTrack[] {
    return [...this.#tracks];
  }

  public addTrack(track: FakeTrack): void {
    if (!this.#tracks.includes(track)) this.#tracks.push(track);
  }

  public removeTrack(track: FakeTrack): void {
    const index = this.#tracks.indexOf(track);
    if (index >= 0) this.#tracks.splice(index, 1);
  }
}

class FakePeerConnection {
  static readonly instances: FakePeerConnection[] = [];

  connectionState: RTCPeerConnectionState = 'new';
  iceConnectionState: RTCIceConnectionState = 'new';
  remoteDescription: RTCSessionDescription | null = null;
  onicecandidate: ((event: RTCPeerConnectionIceEvent) => void) | null = null;
  ontrack: ((event: RTCTrackEvent) => void) | null = null;
  ondatachannel: ((event: RTCDataChannelEvent) => void) | null = null;
  onconnectionstatechange: (() => void) | null = null;
  oniceconnectionstatechange: (() => void) | null = null;

  public constructor() {
    FakePeerConnection.instances.push(this);
  }

  public fireTrack(track: FakeTrack, streams: FakeMediaStream[]): void {
    this.ontrack?.({ track, streams } as unknown as RTCTrackEvent);
  }

  public setRemoteDescription(description: RTCSessionDescriptionInit): Promise<void> {
    this.remoteDescription = description as RTCSessionDescription;
    return Promise.resolve();
  }

  public addIceCandidate(): Promise<void> {
    return Promise.resolve();
  }

  public createAnswer(): Promise<RTCSessionDescriptionInit> {
    return Promise.resolve({ type: 'answer', sdp: 'v=0' });
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
