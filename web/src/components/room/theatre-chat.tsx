import { useRef, useState, type PointerEvent as ReactPointerEvent } from 'react';

import type { ChatLog } from '@/lib/chat/chat-log';
import { ChatPanel } from './chat-panel';

const OVERLAY_WIDTH = 296;
const EDGE_MARGIN = 8;

interface TheatreChatProps {
  log: ChatLog;
  onSend: (text: string) => void;
  onClose: () => void;
  disabled?: boolean;
}

/** The floating, draggable chat overlay shown while theatre mode is active. */
export function TheatreChat({ log, onSend, onClose, disabled = false }: TheatreChatProps) {
  const [position, setPosition] = useState(() => ({
    x: Math.max(EDGE_MARGIN, window.innerWidth - OVERLAY_WIDTH - 24),
    y: 84,
  }));
  const drag = useRef<{ pointerId: number; offsetX: number; offsetY: number } | null>(null);

  const startDrag = (event: ReactPointerEvent<HTMLDivElement>) => {
    if (event.button !== 0) return;
    drag.current = {
      pointerId: event.pointerId,
      offsetX: event.clientX - position.x,
      offsetY: event.clientY - position.y,
    };
    event.currentTarget.setPointerCapture(event.pointerId);
  };

  const moveDrag = (event: ReactPointerEvent<HTMLDivElement>) => {
    const active = drag.current;
    if (!active || active.pointerId !== event.pointerId) return;
    setPosition({
      x: clamp(event.clientX - active.offsetX, EDGE_MARGIN, window.innerWidth - OVERLAY_WIDTH - EDGE_MARGIN),
      y: clamp(event.clientY - active.offsetY, EDGE_MARGIN, window.innerHeight - 160),
    });
  };

  const endDrag = (event: ReactPointerEvent<HTMLDivElement>) => {
    if (drag.current?.pointerId !== event.pointerId) return;
    drag.current = null;
  };

  return (
    <div className="theatre-chat" style={{ left: position.x, top: position.y }}>
      <div
        className="theatre-chat__handle"
        onPointerDown={startDrag}
        onPointerMove={moveDrag}
        onPointerUp={endDrag}
        onPointerCancel={endDrag}
      >
        <span className="theatre-chat__title">Chat</span>
        <span className="theatre-chat__hint">ephemeral</span>
        <span className="theatre-chat__spacer" aria-hidden="true" />
        <button type="button" className="theatre-chat__close" aria-label="Close chat" onClick={onClose}>
          <span aria-hidden="true">×</span>
        </button>
      </div>
      <ChatPanel log={log} onSend={onSend} disabled={disabled} compact />
    </div>
  );
}

function clamp(value: number, minimum: number, maximum: number): number {
  return Math.min(Math.max(value, minimum), Math.max(minimum, maximum));
}
