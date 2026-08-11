export type CaptureResolution = '1440p' | '4k';

export interface CaptureDimensions {
  width: number;
  height: number;
}

export const DEFAULT_CAPTURE_RESOLUTION: CaptureResolution = '1440p';

export const CAPTURE_DIMENSIONS: Readonly<Record<CaptureResolution, CaptureDimensions>> = {
  '1440p': { width: 2560, height: 1440 },
  '4k': { width: 3840, height: 2160 },
};

export function captureDimensions(resolution: CaptureResolution): CaptureDimensions {
  return CAPTURE_DIMENSIONS[resolution];
}
