import { useEffect, useRef } from 'react';

export interface GlobalHotkeyHandlers {
  /** Ctrl/Cmd+K */
  onTogglePalette: () => void;
  /** Ctrl/Cmd+N */
  onCreateRoom: () => void;
  /** Ctrl/Cmd+Shift+A */
  onAddFriend: () => void;
  /** Ctrl/Cmd+Comma */
  onOpenSettings: () => void;
}

function isTypingTarget(target: EventTarget | null): boolean {
  const element = target as HTMLElement | null;
  return (
    !!element &&
    (element.isContentEditable || /^(?:input|textarea|select)$/iu.test(element.tagName))
  );
}

/**
 * App-wide keyboard shortcuts. Every binding requires Ctrl or Cmd. The
 * navigation shortcuts are skipped while a form field is focused so typing is
 * never hijacked; the palette toggle is the exception, so Ctrl/Cmd+K can close
 * the palette from its own search box.
 */
export function useGlobalHotkeys(handlers: GlobalHotkeyHandlers): void {
  const handlersRef = useRef(handlers);
  handlersRef.current = handlers;

  useEffect(() => {
    const onKeyDown = (event: KeyboardEvent) => {
      if (!event.metaKey && !event.ctrlKey) return;
      if (event.altKey) return;
      const key = event.key.toLowerCase();
      if (key === 'k' && !event.shiftKey) {
        event.preventDefault();
        handlersRef.current.onTogglePalette();
      } else if (key === 'n' && !event.shiftKey) {
        if (isTypingTarget(event.target)) return;
        event.preventDefault();
        handlersRef.current.onCreateRoom();
      } else if (key === 'a' && event.shiftKey) {
        if (isTypingTarget(event.target)) return;
        event.preventDefault();
        handlersRef.current.onAddFriend();
      } else if (key === ',' && !event.shiftKey) {
        if (isTypingTarget(event.target)) return;
        event.preventDefault();
        handlersRef.current.onOpenSettings();
      }
    };
    window.addEventListener('keydown', onKeyDown);
    return () => window.removeEventListener('keydown', onKeyDown);
  }, []);
}
