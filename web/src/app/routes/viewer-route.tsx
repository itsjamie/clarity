import { useState } from 'react';
import { useParams, useSearchParams } from 'react-router-dom';

import { FatalError } from '@/components/feedback/fatal-error';
import { ViewerExperience } from '@/features/viewer/components/viewer-experience';
import { takeInviteSecret } from '@/lib/storage/session-storage';

export function ViewerRoute() {
  const { roomId } = useParams();
  const [searchParams] = useSearchParams();
  const [viewerSecret] = useState(() => (roomId ? takeInviteSecret(roomId) : null));
  if (!roomId) return <FatalError title="Room path is invalid" message="Open the full viewer invitation from the presenter." />;
  if (!viewerSecret) {
    return (
      <FatalError
        title="Invitation secret is missing"
        message="Open the complete private viewer link. Its fragment is read once, removed from the address bar, and retained only for this tab session."
      />
    );
  }
  return (
    <ViewerExperience
      roomId={roomId}
      viewerSecret={viewerSecret}
      autoJoin={searchParams.get('access') === 'public'}
    />
  );
}
