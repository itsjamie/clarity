import { isSyntheticCaptureEnabled } from '@/config/environment';
import { storageKeys } from '@/lib/storage/session-storage';
import type { CaptureMode } from '@/lib/webrtc/profiles';

export interface CaptureSettings {
  width?: number;
  height?: number;
  frameRate?: number;
  displaySurface?: string;
  contentHint: string;
  hasAudio: boolean;
}

export interface CaptureResult {
  stream: MediaStream;
  settings: CaptureSettings;
  audioWarning?: string;
}

interface ExtendedDisplayMediaOptions {
  video: MediaTrackConstraints;
  audio: boolean;
  windowAudio?: 'system' | 'window' | 'exclude';
  selfBrowserSurface?: 'include' | 'exclude';
  surfaceSwitching?: 'include' | 'exclude';
}

export class ScreenCaptureManager {
  #stream: MediaStream | null = null;
  #intentionalStop = false;
  readonly #onCaptureEnded: () => void;

  public constructor(onCaptureEnded: () => void) {
    this.#onCaptureEnded = onCaptureEnded;
  }

  public get stream(): MediaStream | null {
    return this.#stream;
  }

  public async start(mode: CaptureMode, includeAudio: boolean): Promise<CaptureResult> {
    if (this.#stream) throw new Error('A capture source is already active.');
    const result = await this.#acquire(mode, includeAudio);
    this.#adopt(result.stream);
    return result;
  }

  public async changeSource(
    mode: CaptureMode,
    includeAudio: boolean,
    replaceTracks: (stream: MediaStream) => Promise<string[]>,
  ): Promise<CaptureResult> {
    const previous = this.#stream;
    const result = await this.#acquire(mode, includeAudio);
    const failures = await replaceTracks(result.stream);
    if (failures.length > 0) {
      result.stream.getTracks().forEach((track) => track.stop());
      throw new Error(`Could not replace the source for ${failures.length} viewer connection(s).`);
    }
    this.#intentionalStop = true;
    previous?.getTracks().forEach((track) => track.stop());
    this.#intentionalStop = false;
    this.#adopt(result.stream);
    return result;
  }

  public stop(): void {
    this.#intentionalStop = true;
    this.#stream?.getTracks().forEach((track) => track.stop());
    this.#stream = null;
    this.#intentionalStop = false;
  }

  async #acquire(mode: CaptureMode, includeAudio: boolean): Promise<CaptureResult> {
    let stream: MediaStream;
    if (
      isSyntheticCaptureEnabled() &&
      window.sessionStorage.getItem(storageKeys.syntheticCapture) === 'enabled'
    ) {
      const { createSyntheticCapture } = await import('@/testing/synthetic-capture');
      stream = createSyntheticCapture({
        width: 1920,
        height: 1080,
        framesPerSecond: mode === 'motion' ? 60 : 30,
      });
    } else {
      if (!window.isSecureContext || !navigator.mediaDevices?.getDisplayMedia) {
        throw new Error('Screen capture requires a supported desktop browser in a secure context.');
      }
      const frameRate = mode === 'motion' ? 60 : 30;
      const options: ExtendedDisplayMediaOptions = {
        video: {
          width: { ideal: 2560 },
          height: { ideal: 1440 },
          frameRate: { ideal: frameRate },
        },
        audio: includeAudio,
        windowAudio: includeAudio ? 'window' : 'exclude',
        selfBrowserSurface: 'exclude',
        surfaceSwitching: 'include',
      };
      stream = await navigator.mediaDevices.getDisplayMedia(options);
    }
    const videoTrack = stream.getVideoTracks()[0];
    if (!videoTrack) {
      stream.getTracks().forEach((track) => track.stop());
      throw new Error('The selected source did not provide a video track.');
    }
    applyContentHint(videoTrack, mode);
    const trackSettings = videoTrack.getSettings();
    return {
      stream,
      settings: {
        width: trackSettings.width,
        height: trackSettings.height,
        frameRate: trackSettings.frameRate,
        displaySurface: trackSettings.displaySurface,
        contentHint: videoTrack.contentHint,
        hasAudio: stream.getAudioTracks().length > 0,
      },
      audioWarning:
        includeAudio && stream.getAudioTracks().length === 0
          ? 'The browser did not provide shared audio. Video sharing will continue.'
          : undefined,
    };
  }

  #adopt(stream: MediaStream): void {
    this.#stream = stream;
    const videoTrack = stream.getVideoTracks()[0];
    videoTrack?.addEventListener('ended', () => {
      if (!this.#intentionalStop && this.#stream === stream) this.#onCaptureEnded();
    }, { once: true });
  }
}

function applyContentHint(track: MediaStreamTrack, mode: CaptureMode): void {
  const preferred = mode === 'text' ? 'text' : 'motion';
  track.contentHint = preferred;
  if (mode === 'text' && track.contentHint !== 'text') track.contentHint = 'detail';
}
