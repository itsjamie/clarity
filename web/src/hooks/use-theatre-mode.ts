import { useCallback, useEffect, useState } from 'react';

const THEATRE_TOGGLE_EVENT = 'clarity:theatre-toggle';

/**
 * Toggles theatre mode in whichever room view is mounted, from code that has
 * no handle on it (the command palette). A no-op outside a room.
 */
export function requestTheatreToggle(): void {
  window.dispatchEvent(new Event(THEATRE_TOGGLE_EVENT));
}

/**
 * Theatre mode collapses the shell and side panel around the stage. `T`
 * toggles it and `Escape` leaves it; keystrokes inside form fields are
 * ignored so typing "t" in chat never flips the layout.
 */
export function useTheatreMode(): {
  theatre: boolean;
  toggleTheatre: () => void;
  exitTheatre: () => void;
} {
  const [theatre, setTheatre] = useState(false);
  const toggleTheatre = useCallback(() => setTheatre((value) => !value), []);
  const exitTheatre = useCallback(() => setTheatre(false), []);

  useEffect(() => {
    const onKeyDown = (event: KeyboardEvent) => {
      if (event.metaKey || event.ctrlKey || event.altKey) return;
      const target = event.target as HTMLElement | null;
      if (
        target &&
        (target.isContentEditable || /^(?:input|textarea|select)$/iu.test(target.tagName))
      ) {
        return;
      }
      if (event.key === 't' || event.key === 'T') {
        event.preventDefault();
        setTheatre((value) => !value);
      } else if (event.key === 'Escape') {
        setTheatre(false);
      }
    };
    const onToggleRequest = () => setTheatre((value) => !value);
    window.addEventListener('keydown', onKeyDown);
    window.addEventListener(THEATRE_TOGGLE_EVENT, onToggleRequest);
    return () => {
      window.removeEventListener('keydown', onKeyDown);
      window.removeEventListener(THEATRE_TOGGLE_EVENT, onToggleRequest);
    };
  }, []);

  return { theatre, toggleTheatre, exitTheatre };
}
