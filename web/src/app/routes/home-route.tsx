import { useOutletContext } from 'react-router-dom';

import { useSessionState } from '@/hooks/use-session-state';
import {
  contactsStore,
  presenceStore,
} from '@/lib/presence/presence-service';
import {
  friendRows,
  initials,
  liveRooms,
  type FriendRow,
} from '@/features/shell/lib/friend-rows';
import type { ShellOutletContext } from './shell-route';

export function HomeRoute() {
  const { openCreateRoom } = useOutletContext<ShellOutletContext>();
  const contacts = useSessionState(contactsStore);
  const presence = useSessionState(presenceStore);
  const rooms = liveRooms(friendRows(contacts.contacts, presence.friends));

  return (
    <div className="shell-page shell-home">
      {rooms.length === 0 ? (
        <div className="shell-empty">
          <h1>Nothing live yet</h1>
          <p>
            When a friend opens a room it shows up here. Start your own, or add
            friends by trading codes so you can see each other's rooms.
          </p>
          <div className="shell-empty__actions">
            <button type="button" className="shell-button shell-button--accent" onClick={openCreateRoom}>
              Create room
            </button>
          </div>
        </div>
      ) : (
        <div className="shell-home__cards">
          {rooms.map((row) =>
            row.hosting!.sharingState === 'live' ? (
              <LiveCard key={row.code} row={row} />
            ) : (
              <IdleCard key={row.code} row={row} />
            ),
          )}
        </div>
      )}
    </div>
  );
}

function LiveCard({ row }: { row: FriendRow }) {
  const hosting = row.hosting!;
  return (
    <article className="room-card room-card--live">
      <div className="room-card__preview">
        <span className="room-card__live-pill">
          <i className="pulse-dot" aria-hidden="true" />
          LIVE
        </span>
      </div>
      <div className="room-card__body">
        <div className="room-card__copy">
          <strong>{row.name} is sharing</strong>
          <span>
            {hosting.viewerCount === 0
              ? 'Nobody watching yet'
              : `${hosting.viewerCount} ${hosting.viewerCount === 1 ? 'viewer' : 'viewers'} watching`}
          </span>
        </div>
        <a className="shell-button shell-button--accent" href={hosting.viewerUrl}>
          Join
        </a>
      </div>
    </article>
  );
}

function IdleCard({ row }: { row: FriendRow }) {
  const hosting = row.hosting!;
  const paused = hosting.sharingState === 'paused';
  return (
    <article className="room-card room-card--idle">
      <span className="friend-row__avatar friend-row__avatar--large" aria-hidden="true">
        {initials(row.name)}
      </span>
      <div className="room-card__copy">
        <strong>{row.name}'s room</strong>
        <span>
          {paused ? 'Sharing paused' : 'Open, nobody sharing yet'}
          {hosting.viewerCount > 0 ? ` · ${hosting.viewerCount + 1} here` : ''}
        </span>
      </div>
      <a className="shell-button shell-button--ghost" href={hosting.viewerUrl}>
        Join
      </a>
    </article>
  );
}
