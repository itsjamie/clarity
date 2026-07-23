const SYNTHETIC_CAPTURE_TEST_ONLY_MARKER = 'CLARITY_SYNTHETIC_CAPTURE_TEST_ONLY';

interface SyntheticCaptureOptions {
  width: number;
  height: number;
  framesPerSecond: number;
}

export function createSyntheticCapture(options: SyntheticCaptureOptions): MediaStream {
  if (
    import.meta.env.MODE !== 'test' ||
    import.meta.env.VITE_ENABLE_SYNTHETIC_CAPTURE !== 'true'
  ) {
    throw new Error('Synthetic capture is available only in an explicit test build.');
  }
  void SYNTHETIC_CAPTURE_TEST_ONLY_MARKER;
  const canvas = document.createElement('canvas');
  canvas.width = options.width;
  canvas.height = options.height;
  const context = canvas.getContext('2d');
  if (!context) throw new Error('Canvas rendering is unavailable.');
  let frame = 0;
  const draw = (): void => {
    const hue = (frame * 3) % 360;
    context.fillStyle = '#111117';
    context.fillRect(0, 0, canvas.width, canvas.height);
    context.fillStyle = '#ececf2';
    context.font = '700 54px system-ui';
    context.fillText('Clarity Share synthetic source', 64, 96);
    context.font = '24px ui-monospace, monospace';
    context.fillStyle = '#b8b8c6';
    for (let row = 0; row < 18; row += 1) {
      context.fillText(
        `${String(row + 1).padStart(2, '0')}  const detail = "small readable text ${frame % 1000}";`,
        72,
        160 + row * 36,
      );
    }
    context.fillStyle = `hsl(${hue} 72% 58%)`;
    context.fillRect(80 + ((frame * 28) % Math.max(1, canvas.width - 360)), canvas.height - 250, 280, 120);
    context.strokeStyle = '#7777ff';
    context.lineWidth = 8;
    context.beginPath();
    context.arc(canvas.width - 240, 220, 110, frame * 0.12, frame * 0.12 + Math.PI * 1.5);
    context.stroke();
    frame += 1;
  };
  draw();
  const interval = window.setInterval(draw, 1_000 / options.framesPerSecond);
  const stream = canvas.captureStream(options.framesPerSecond);
  const track = stream.getVideoTracks()[0];
  const monitor = window.setInterval(() => {
    if (!track || track.readyState === 'ended') {
      window.clearInterval(interval);
      window.clearInterval(monitor);
    }
  }, 250);
  return stream;
}
