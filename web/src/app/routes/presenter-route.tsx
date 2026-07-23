import { useParams } from 'react-router-dom';

import { FatalError } from '@/components/feedback/fatal-error';
import { PresenterWorkspace } from '@/features/presenter/components/presenter-workspace';
import { storageKeys } from '@/lib/storage/session-storage';

export function PresenterRoute() {
  const { roomId } = useParams();
  if (!roomId) return <FatalError title="Room path is invalid" message="Create a new room to begin sharing." />;
  const presenterSecret = window.sessionStorage.getItem(storageKeys.presenterSecret(roomId));
  const viewerUrl = window.sessionStorage.getItem(storageKeys.viewerUrl(roomId));
  if (!presenterSecret || !viewerUrl) {
    return (
      <FatalError
        title="Presenter credentials are unavailable"
        message="Presenter credentials live only in this tab’s session. Create a new room if the tab session was cleared."
      />
    );
  }
  return <PresenterWorkspace roomId={roomId} presenterSecret={presenterSecret} viewerUrl={viewerUrl} />;
}
