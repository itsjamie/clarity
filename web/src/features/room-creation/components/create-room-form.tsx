import { useState, type FormEvent } from 'react';
import { useNavigate } from 'react-router-dom';

import { Button } from '@/components/ui/button';
import {
  DEFAULT_APPROVAL_VIEWERS,
  MAXIMUM_VIEWERS,
  MINIMUM_VIEWERS,
  PUBLIC_ROOM_VIEWERS,
  isValidViewerLimit,
} from '@/config/room';
import type { RoomAccessPolicy } from '@/generated/protocol';
import { storageKeys } from '@/lib/storage/session-storage';
import { createRoom } from '../api/create-room';
import { useBrowserCapabilities } from '../hooks/use-browser-capabilities';

export function CreateRoomForm() {
  const navigate = useNavigate();
  const capabilities = useBrowserCapabilities();
  const [approvalViewerLimit, setApprovalViewerLimit] = useState(String(DEFAULT_APPROVAL_VIEWERS));
  const [expiresInSeconds, setExpiresInSeconds] = useState(7_200);
  const [accessPolicy, setAccessPolicy] = useState<RoomAccessPolicy>('public');
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const parsedApprovalLimit = Number(approvalViewerLimit);
  const viewerLimitValid = accessPolicy === 'public' || isValidViewerLimit(parsedApprovalLimit);
  const maximumViewers = accessPolicy === 'public' ? PUBLIC_ROOM_VIEWERS : parsedApprovalLimit;

  const submit = async (event: FormEvent<HTMLFormElement>) => {
    event.preventDefault();
    setBusy(true);
    setError(null);
    try {
      const room = await createRoom({ maximumViewers, expiresInSeconds, accessPolicy });
      window.sessionStorage.setItem(storageKeys.presenterSecret(room.roomId), room.presenterSecret);
      window.sessionStorage.setItem(storageKeys.viewerUrl(room.roomId), room.viewerUrl);
      await navigate(room.presenterPath);
    } catch (caught) {
      setError(caught instanceof Error ? caught.message : 'The room could not be created.');
    } finally {
      setBusy(false);
    }
  };

  return (
    <form className="create-room" onSubmit={(event) => void submit(event)}>
      <div className="create-room__heading">
        <span className="step-index" aria-hidden="true">01</span>
        <div>
          <h2>Set the room boundary</h2>
          <p>Rooms expire automatically and never survive a server restart.</p>
        </div>
      </div>

      <fieldset className="choice-group">
        <legend>Viewer access</legend>
        <div className="segmented-control">
          <AccessOption
            active={accessPolicy === 'public'}
            title="Public link"
            detail="Anyone with the secure link joins immediately."
            onClick={() => setAccessPolicy('public')}
          />
          <AccessOption
            active={accessPolicy === 'approvalRequired'}
            title="Approval required"
            detail="Viewers wait until you admit them."
            onClick={() => setAccessPolicy('approvalRequired')}
          />
        </div>
      </fieldset>

      <div className="field-row">
        <label className="field">
          <span>
            Viewer limit
            <small>{accessPolicy === 'public' ? 'Public links use 10' : 'From 1 to 10'}</small>
          </span>
          <input
            type="number"
            min={MINIMUM_VIEWERS}
            max={MAXIMUM_VIEWERS}
            step="1"
            inputMode="numeric"
            value={accessPolicy === 'public' ? PUBLIC_ROOM_VIEWERS : approvalViewerLimit}
            disabled={accessPolicy === 'public'}
            required
            onChange={(event) => setApprovalViewerLimit(event.target.value)}
          />
        </label>
        <label className="field">
          <span>Room lifetime</span>
          <select
            value={expiresInSeconds}
            onChange={(event) => setExpiresInSeconds(Number(event.target.value))}
          >
            <option value={3_600}>1 hour</option>
            <option value={7_200}>2 hours</option>
            <option value={14_400}>4 hours</option>
            <option value={28_800}>8 hours</option>
          </select>
        </label>
      </div>

      {capabilities.reason ? <p className="inline-notice inline-notice--warning">{capabilities.reason}</p> : null}
      {error ? <p className="inline-notice inline-notice--danger" role="alert">{error}</p> : null}

      <Button type="submit" variant="primary" disabled={!capabilities.canPresent || busy || !viewerLimitValid}>
        {busy ? 'Creating secure room…' : 'Create room'}
      </Button>
    </form>
  );
}

function AccessOption(props: {
  active: boolean;
  title: string;
  detail: string;
  onClick: () => void;
}) {
  return (
    <button
      type="button"
      className={props.active ? 'mode-option mode-option--active' : 'mode-option'}
      aria-pressed={props.active}
      onClick={props.onClick}
    >
      <span className="mode-option__indicator" aria-hidden="true" />
      <span className="mode-option__copy">
        <strong>{props.title}</strong>
        <span>{props.detail}</span>
      </span>
    </button>
  );
}
