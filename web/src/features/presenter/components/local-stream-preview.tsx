import { useEffect, useRef } from 'react';

interface LocalStreamPreviewProps {
  stream: MediaStream | null;
  active: boolean;
  paused: boolean;
}

export function LocalStreamPreview({ stream, active, paused }: LocalStreamPreviewProps) {
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
    <div
      className={`presenter-stage__preview${active ? ' presenter-stage__preview--live' : ''}${paused ? ' presenter-stage__preview--paused' : ''}`}
    >
      {(active || paused) && stream ? (
        <video
          ref={videoRef}
          className="presenter-stage__video"
          aria-label={paused ? 'Local preview of the sharing paused slate' : 'Local preview of the shared screen'}
          autoPlay
          muted
          playsInline
        />
      ) : (
        <div className="presenter-stage__placeholder">
          <strong>{active ? 'Preparing preview' : paused ? 'Sharing paused' : 'Screen preview'}</strong>
          <span>
            {active
              ? 'The shared source will appear here.'
              : paused
                ? 'Viewers remain connected. Choose a source to resume.'
                : 'Viewers see this feed after sharing starts.'}
          </span>
        </div>
      )}
      {active || paused ? (
        <span className={`presenter-stage__live-badge${paused ? ' presenter-stage__live-badge--paused' : ''}`}>
          <i aria-hidden="true" /> {paused ? 'Paused' : 'Live'}
        </span>
      ) : null}
    </div>
  );
}
