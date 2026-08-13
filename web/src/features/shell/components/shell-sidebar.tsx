import { useState, type FormEvent } from 'react';
import { NavLink, useNavigate } from 'react-router-dom';

import { useSessionState } from '@/hooks/use-session-state';
import {
  contactsStore,
  dismissedRequestsStore,
  identityStore,
  presenceStore,
} from '@/lib/presence/presence-service';
import { openRequests } from '@/lib/identity/dismissed-requests';
import {
  formatLastSeen,
  friendRows,
  initials,
  liveRooms,
} from '../lib/friend-rows';
import { parseInviteLink } from '../lib/invite-link';

export function ShellSidebar({
  onCreateRoom,
  onOpenPalette,
}: {
  onCreateRoom: () => void;
  onOpenPalette: () => void;
}) {
  const identity = useSessionState(identityStore);
  const contacts = useSessionState(contactsStore);
  const presence = useSessionState(presenceStore);
  const dismissed = useSessionState(dismissedRequestsStore);
  const rows = friendRows(contacts.contacts, presence.friends);
  const live = liveRooms(rows);
  const invites = openRequests(presence.requests, contacts.contacts, dismissed.codes);

  return (
    <aside className="shell-sidebar">
      <div className="shell-sidebar__actions">
        <button type="button" className="shell-button shell-button--accent" onClick={onCreateRoom}>
          Create room <kbd>⌘N</kbd>
        </button>
        <JoinByLink />
        <button type="button" className="shell-sidebar__palette" onClick={onOpenPalette}>
          Search or run · ⌘K
        </button>
      </div>

      <div className="shell-sidebar__scroll">
        <div className="shell-sidebar__heading">
          <span>Live now</span>
          <span>{live.length}</span>
        </div>
        {live.length === 0 ? (
          <p className="shell-sidebar__quiet">No friend rooms open.</p>
        ) : (
          live.map((row) => <LiveRoomCard key={row.code} row={row} />)
        )}

        <div className="shell-sidebar__heading">
          <span>Friends</span>
          <span>{rows.length}</span>
        </div>
        <div className="shell-sidebar__friends">
          {rows.map((row) => (
            <div key={row.code} className={row.online ? 'friend-row' : 'friend-row friend-row--offline'}>
              <span className="friend-row__avatar" aria-hidden="true">{initials(row.name)}</span>
              <span className="friend-row__name">{row.name}</span>
              {row.online ? (
                <i className="friend-row__dot" aria-label="Online" />
              ) : (
                <span className="friend-row__seen">
                  {/* Never-mutual contacts are still an invite, not "away". */}
                  {row.confirmed ? formatLastSeen(row) : 'invited'}
                </span>
              )}
            </div>
          ))}
        </div>
        <NavLink to="/friends" className="shell-sidebar__add-friend">
          {invites.length === 0
            ? 'Add a friend'
            : `Add a friend · ${invites.length} ${invites.length === 1 ? 'invite' : 'invites'}`}
        </NavLink>
      </div>

      <NavLink to="/settings" className="shell-sidebar__identity">
        <span className="friend-row__avatar friend-row__avatar--self" aria-hidden="true">
          {initials(identity.displayName || 'You')}
        </span>
        <span className="shell-sidebar__identity-copy">
          <strong>
            {identity.displayName ? `${identity.displayName} · ${identity.deviceLabel}` : 'You'}
          </strong>
          <span>{identity.friendCode ?? 'no identity yet'}</span>
        </span>
      </NavLink>
    </aside>
  );
}

function LiveRoomCard({ row }: { row: ReturnType<typeof friendRows>[number] }) {
  const hosting = row.hosting!;
  const sharing = hosting.sharingState === 'live';
  // Mirrors the desktop sidebar: a paused share is a room the presenter is
  // coming back to, not an idle room.
  const stateLine = sharing
    ? `sharing · ${hosting.viewerCount + 1} here`
    : hosting.sharingState === 'paused'
      ? `paused · ${hosting.viewerCount + 1} here`
      : `idle room · ${hosting.viewerCount + 1} here`;
  return (
    <article className={sharing ? 'live-card live-card--sharing' : 'live-card'}>
      <div className="live-card__row">
        <span className="friend-row__avatar friend-row__avatar--self" aria-hidden="true">
          {initials(row.name)}
        </span>
        <span className="live-card__copy">
          <strong>{row.name}</strong>
          <span className={sharing ? 'live-card__state live-card__state--live' : 'live-card__state'}>
            {sharing ? <i className="pulse-dot" aria-hidden="true" /> : null}
            {stateLine}
          </span>
        </span>
      </div>
      <a className="shell-button shell-button--join" href={hosting.viewerUrl}>
        Join
      </a>
    </article>
  );
}

function JoinByLink() {
  const navigate = useNavigate();
  const [open, setOpen] = useState(false);
  const [link, setLink] = useState('');
  const [error, setError] = useState(false);

  const submit = (event: FormEvent<HTMLFormElement>) => {
    event.preventDefault();
    const invite = parseInviteLink(link);
    if (!invite) {
      setError(true);
      return;
    }
    setOpen(false);
    setLink('');
    setError(false);
    if (invite.sameOrigin) {
      void navigate(invite.appPath);
    } else {
      window.location.assign(invite.url);
    }
  };

  if (!open) {
    return (
      <button type="button" className="shell-button shell-button--ghost" onClick={() => setOpen(true)}>
        Join by link
      </button>
    );
  }
  return (
    <form className="join-by-link" onSubmit={submit}>
      <input
        autoFocus
        type="text"
        placeholder="Paste an invite link"
        aria-label="Invite link"
        aria-invalid={error}
        value={link}
        onChange={(event) => {
          setLink(event.target.value);
          setError(false);
        }}
        onKeyDown={(event) => {
          if (event.key === 'Escape') setOpen(false);
        }}
      />
      {error ? <p className="join-by-link__error" role="alert">That is not a Clarity invite link.</p> : null}
      <div className="join-by-link__actions">
        <button type="submit" className="shell-button shell-button--accent">Join</button>
        <button type="button" className="shell-button shell-button--ghost" onClick={() => setOpen(false)}>
          Cancel
        </button>
      </div>
    </form>
  );
}
