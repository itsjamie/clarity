import { useParams } from 'react-router-dom';

import { FatalError } from '@/components/feedback/fatal-error';
import { PresenterWorkspace } from '@/features/presenter/components/presenter-workspace';
import { loadPresenterCredentials } from '@/lib/storage/session-storage';

export function PresenterRoute() {
  const { roomId } = useParams();
  if (!roomId) return <FatalError title="Room path is invalid" message="Create a new room to begin sharing." />;
  const credentials = loadPresenterCredentials(roomId);
  if (!credentials) {
    return (
      <FatalError
        title="Presenter credentials are unavailable"
        message="Presenter credentials live on the device that created the room. Create a new room if this browser's storage was cleared."
      />
    );
  }
  return (
    <PresenterWorkspace
      roomId={roomId}
      presenterSecret={credentials.presenterSecret}
      viewerUrl={credentials.viewerUrl}
    />
  );
}
