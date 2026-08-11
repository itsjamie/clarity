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
import { useSessionState } from '@/hooks/use-session-state';
import { contactsStore } from '@/lib/presence/presence-service';
import { loadAppSettings } from '@/lib/settings/app-settings';
import {
  storePresenterCredentials,
  storeRoomCaptureMode,
} from '@/lib/storage/session-storage';
import type { CaptureMode } from '@/lib/webrtc/profiles';
import { createRoom } from '../api/create-room';
import { useBrowserCapabilities } from '../hooks/use-browser-capabilities';

export function CreateRoomForm() {
  const navigate = useNavigate();
  const capabilities = useBrowserCapabilities();
  // Only confirmed-mutual contacts can be allowlisted, matching the desktop
  // client: a code typed in but never confirmed is not yet a friend.
  const contacts = useSessionState(contactsStore).contacts.filter(
    (contact) => contact.confirmed,
  );
  const [approvalViewerLimit, setApprovalViewerLimit] = useState(String(DEFAULT_APPROVAL_VIEWERS));
  const [expiresInSeconds, setExpiresInSeconds] = useState(7_200);
  const [accessPolicy, setAccessPolicy] = useState<RoomAccessPolicy>('public');
  const [captureMode, setCaptureMode] = useState<CaptureMode>(() => loadAppSettings().captureMode);
  const [allowedCodes, setAllowedCodes] = useState<readonly string[]>([]);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const parsedApprovalLimit = Number(approvalViewerLimit);
  const viewerLimitValid = accessPolicy === 'public' || isValidViewerLimit(parsedApprovalLimit);
  const maximumViewers = accessPolicy === 'public' ? PUBLIC_ROOM_VIEWERS : parsedApprovalLimit;
  const friendsSelectionValid = accessPolicy !== 'friendsOnly' || allowedCodes.length > 0;

  const toggleCode = (code: string) => {
    setAllowedCodes((codes) =>
      codes.includes(code) ? codes.filter((existing) => existing !== code) : [...codes, code],
    );
  };

  const submit = async (event: FormEvent<HTMLFormElement>) => {
    event.preventDefault();
    setBusy(true);
    setError(null);
    try {
      const room = await createRoom({
        maximumViewers,
        expiresInSeconds,
        accessPolicy,
        ...(accessPolicy === 'friendsOnly' ? { allowedFriendCodes: [...allowedCodes] } : {}),
      });
      storePresenterCredentials(room.roomId, {
        presenterSecret: room.presenterSecret,
        viewerUrl: room.viewerUrl,
      });
      storeRoomCaptureMode(room.roomId, captureMode);
      await navigate(room.presenterPath);
    } catch (caught) {
      setError(caught instanceof Error ? caught.message : 'The room could not be created.');
    } finally {
      setBusy(false);
    }
  };

  return (
    <form className="create-room" onSubmit={(event) => void submit(event)}>
      <fieldset className="choice-group">
        <legend>Who can join</legend>
        <div className="segmented-control segmented-control--stack">
          <ChoiceOption
            active={accessPolicy === 'public'}
            title="Anyone with the link"
            detail="The secure invite link admits viewers immediately."
            onClick={() => setAccessPolicy('public')}
          />
          <ChoiceOption
            active={accessPolicy === 'approvalRequired'}
            title="Ask me first"
            detail="Viewers wait until you approve each one."
            onClick={() => setAccessPolicy('approvalRequired')}
          />
          <ChoiceOption
            active={accessPolicy === 'friendsOnly'}
            title="Friends only"
            detail="Only identity-proven friend codes you pick."
            onClick={() => setAccessPolicy('friendsOnly')}
          />
        </div>
      </fieldset>

      {accessPolicy === 'friendsOnly' ? (
        <fieldset className="choice-group create-room__friends">
          <legend>Which friends</legend>
          {contacts.length === 0 ? (
            <p className="inline-notice inline-notice--warning">
              Add friends first: a friends-only room needs at least one confirmed friend on its
              allowlist.
            </p>
          ) : (
            <ul className="create-room__friend-list">
              {contacts.map((contact) => (
                <li key={contact.code}>
                  <label>
                    <input
                      type="checkbox"
                      checked={allowedCodes.includes(contact.code)}
                      onChange={() => toggleCode(contact.code)}
                    />
                    <span className="create-room__friend-name">{contact.name}</span>
                    <code>{contact.code}</code>
                  </label>
                </li>
              ))}
            </ul>
          )}
        </fieldset>
      ) : null}

      <fieldset className="choice-group">
        <legend>Capture profile</legend>
        <div className="segmented-control">
          <ChoiceOption
            active={captureMode === 'text'}
            title="Text"
            detail="Sharp at 30 fps"
            onClick={() => setCaptureMode('text')}
          />
          <ChoiceOption
            active={captureMode === 'motion'}
            title="Motion"
            detail="Smooth at 60 fps"
            onClick={() => setCaptureMode('motion')}
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
          <span>Room expires in</span>
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

      <Button
        type="submit"
        variant="primary"
        disabled={!capabilities.canPresent || busy || !viewerLimitValid || !friendsSelectionValid}
      >
        {busy ? 'Opening room…' : 'Open room'}
      </Button>
    </form>
  );
}

function ChoiceOption(props: {
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
