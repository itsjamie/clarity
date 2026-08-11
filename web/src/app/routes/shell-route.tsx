import { useEffect } from 'react';
import { Navigate, Outlet, useOutletContext } from 'react-router-dom';

import { ShellSidebar } from '@/features/shell/components/shell-sidebar';
import { useSessionState } from '@/hooks/use-session-state';
import {
  ensurePresenceStarted,
  identityStore,
} from '@/lib/presence/presence-service';
import type { RootOutletContext } from './root-route';
import '@/styles/shell.css';

export function ShellRoute() {
  const identity = useSessionState(identityStore);
  const { openCreateRoom, openPalette } = useOutletContext<RootOutletContext>();

  useEffect(() => {
    ensurePresenceStarted();
  }, []);

  if (identity.status === 'absent') return <Navigate to="/onboarding" replace />;

  return (
    <div className="shell">
      <ShellSidebar onCreateRoom={openCreateRoom} onOpenPalette={openPalette} />
      <main className="shell__main">
        {identity.status === 'unsupported' ? (
          <p className="shell__banner" role="alert">
            This browser cannot create an Ed25519 identity key, so friends and
            presence are unavailable. Rooms you are invited to still work.
          </p>
        ) : null}
        <Outlet context={{ openCreateRoom } satisfies ShellOutletContext} />
      </main>
    </div>
  );
}

export interface ShellOutletContext {
  openCreateRoom: () => void;
}
