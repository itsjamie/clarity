import { useCallback, useState } from 'react';
import { Outlet, useNavigate } from 'react-router-dom';

import { CommandPalette } from '@/features/shell/components/command-palette';
import { CreateRoomDialog } from '@/features/shell/components/create-room-dialog';
import { useGlobalHotkeys } from '@/hooks/use-global-hotkeys';
import '@/styles/shell.css';

/**
 * Pathless layout above every route. It owns the overlays that must work
 * anywhere in the app: the create-room dialog (sidebar button, palette, or
 * Ctrl/Cmd+N) and the Ctrl/Cmd+K command palette.
 */
export function RootRoute() {
  const navigate = useNavigate();
  const [createOpen, setCreateOpen] = useState(false);
  const [paletteOpen, setPaletteOpen] = useState(false);

  const openCreateRoom = useCallback(() => {
    setPaletteOpen(false);
    setCreateOpen(true);
  }, []);

  useGlobalHotkeys({
    onTogglePalette: () => setPaletteOpen((open) => !open),
    onCreateRoom: openCreateRoom,
    onAddFriend: () => {
      setPaletteOpen(false);
      void navigate('/friends');
    },
    onOpenSettings: () => {
      setPaletteOpen(false);
      void navigate('/settings');
    },
  });

  return (
    <>
      <Outlet
        context={{ openCreateRoom, openPalette: () => setPaletteOpen(true) } satisfies RootOutletContext}
      />
      {createOpen ? <CreateRoomDialog onClose={() => setCreateOpen(false)} /> : null}
      {paletteOpen ? (
        <CommandPalette onClose={() => setPaletteOpen(false)} onCreateRoom={openCreateRoom} />
      ) : null}
    </>
  );
}

export interface RootOutletContext {
  openCreateRoom: () => void;
  openPalette: () => void;
}
