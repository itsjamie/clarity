import { useEffect, useRef } from 'react';

interface LocalStreamPreviewProps {
  stream: MediaStream | null;
  active: boolean;
}

export function LocalStreamPreview({ stream, active }: LocalStreamPreviewProps) {
  const videoRef = useRef<HTMLVideoElement>(null);

  useEffect(() => {
    const video = videoRef.current;
    if (!video) return;
    video.srcObject = stream;
    if (stream) void video.play().catch(() => undefined);
    return () => {
      video.srcObject = null;
    };
  }, [stream]);

  return (
    <div className={`presenter-stage__preview${active ? ' presenter-stage__preview--live' : ''}`}>
      {active && stream ? (
        <video
          ref={videoRef}
          className="presenter-stage__video"
          aria-label="Local preview of the shared screen"
          autoPlay
          muted
          playsInline
        />
      ) : (
        <div className="presenter-stage__placeholder">
          <strong>{active ? 'Preparing preview' : 'Screen preview'}</strong>
          <span>{active ? 'The shared source will appear here.' : 'Viewers see this feed after sharing starts.'}</span>
        </div>
      )}
      {active ? (
        <span className="presenter-stage__live-badge">
          <i aria-hidden="true" /> Live
        </span>
      ) : null}
    </div>
  );
}
