import { useEffect, useMemo, useRef, useState, type KeyboardEvent } from 'react';
import { useLocation, useNavigate } from 'react-router-dom';

import { useSessionState } from '@/hooks/use-session-state';
import { requestTheatreToggle } from '@/hooks/use-theatre-mode';
import { contactsStore, presenceStore } from '@/lib/presence/presence-service';
import { friendRows, liveRooms } from '../lib/friend-rows';
import { parseInviteLink, type InviteTarget } from '../lib/invite-link';
import { ROOM_ITEM_PREFIX, paletteItems, type PaletteItem } from '../lib/palette-actions';

interface CommandPaletteProps {
  onClose: () => void;
  onCreateRoom: () => void;
}

export function CommandPalette({ onClose, onCreateRoom }: CommandPaletteProps) {
  const navigate = useNavigate();
  const location = useLocation();
  const contacts = useSessionState(contactsStore);
  const presence = useSessionState(presenceStore);
  const inputRef = useRef<HTMLInputElement>(null);
  const [query, setQuery] = useState('');
  const [linkMode, setLinkMode] = useState(false);
  const [linkError, setLinkError] = useState(false);
  const [activeIndex, setActiveIndex] = useState(0);

  const rooms = useMemo(
    () => liveRooms(friendRows(contacts.contacts, presence.friends)),
    [contacts.contacts, presence.friends],
  );
  const inRoom =
    location.pathname.startsWith('/present/') || location.pathname.startsWith('/r/');
  const pastedInvite = linkMode ? null : parseInviteLink(query);
  const items = useMemo<PaletteItem[]>(() => {
    if (linkMode) return [];
    if (pastedInvite) {
      return [
        { id: 'open-invite', label: 'Open invite link', hint: pastedInvite.roomId },
        ...paletteItems(query, rooms, inRoom),
      ];
    }
    return paletteItems(query, rooms, inRoom);
  }, [inRoom, linkMode, pastedInvite, query, rooms]);
  const active = items[Math.min(activeIndex, items.length - 1)];

  useEffect(() => {
    inputRef.current?.focus();
  }, [linkMode]);

  useEffect(() => {
    setActiveIndex(0);
  }, [query, linkMode]);

  const goToInvite = (invite: InviteTarget) => {
    onClose();
    if (invite.sameOrigin) {
      void navigate(invite.appPath);
    } else {
      window.location.assign(invite.url);
    }
  };

  const run = (item: PaletteItem) => {
    if (item.id === 'create-room') {
      onCreateRoom();
      return;
    }
    if (item.id === 'add-friend') {
      onClose();
      void navigate('/friends');
      return;
    }
    if (item.id === 'open-settings') {
      onClose();
      void navigate('/settings');
      return;
    }
    if (item.id === 'join-link') {
      setLinkMode(true);
      setQuery('');
      setLinkError(false);
      return;
    }
    if (item.id === 'toggle-theatre') {
      onClose();
      requestTheatreToggle();
      return;
    }
    if (item.id === 'open-invite' && pastedInvite) {
      goToInvite(pastedInvite);
      return;
    }
    if (item.id.startsWith(ROOM_ITEM_PREFIX)) {
      const code = item.id.slice(ROOM_ITEM_PREFIX.length);
      const hosting = rooms.find((row) => row.code === code)?.hosting;
      if (!hosting) return;
      const invite = parseInviteLink(hosting.viewerUrl);
      if (invite) goToInvite(invite);
      else window.location.assign(hosting.viewerUrl);
    }
  };

  const submitLink = () => {
    const invite = parseInviteLink(query);
    if (!invite) {
      setLinkError(true);
      return;
    }
    goToInvite(invite);
  };

  const onKeyDown = (event: KeyboardEvent<HTMLInputElement>) => {
    if (event.key === 'Escape') {
      event.preventDefault();
      // Keep Escape from also closing whatever sits under the palette.
      event.stopPropagation();
      if (linkMode) {
        setLinkMode(false);
        setQuery('');
        setLinkError(false);
      } else {
        onClose();
      }
      return;
    }
    if (linkMode) {
      if (event.key === 'Enter') {
        event.preventDefault();
        submitLink();
      }
      return;
    }
    if (event.key === 'ArrowDown') {
      event.preventDefault();
      setActiveIndex((index) => Math.min(index + 1, items.length - 1));
    } else if (event.key === 'ArrowUp') {
      event.preventDefault();
      setActiveIndex((index) => Math.max(index - 1, 0));
    } else if (event.key === 'Enter') {
      event.preventDefault();
      if (active) run(active);
    }
  };

  return (
    <div className="shell-overlay shell-overlay--palette" onClick={onClose}>
      <div
        className="palette"
        role="dialog"
        aria-modal="true"
        aria-label="Command palette"
        onClick={(event) => event.stopPropagation()}
      >
        <input
          ref={inputRef}
          type="text"
          className="palette__input"
          placeholder={
            linkMode ? 'Paste an invite link' : 'Jump to a friend, room, or setting…'
          }
          aria-label={linkMode ? 'Invite link' : 'Search commands'}
          value={query}
          onChange={(event) => {
            setQuery(event.target.value);
            setLinkError(false);
          }}
          onKeyDown={onKeyDown}
        />
        {linkMode ? (
          <div className="palette__list">
            {linkError ? (
              <p className="palette__empty" role="alert">That is not a Clarity invite link.</p>
            ) : (
              <p className="palette__empty">Press Enter to join, Escape to go back.</p>
            )}
          </div>
        ) : (
          <div className="palette__list">
            <div className="palette__eyebrow">Actions</div>
            {items.length === 0 ? (
              <p className="palette__empty">Nothing matches.</p>
            ) : (
              items.map((item) => (
                <button
                  key={item.id}
                  type="button"
                  className={
                    item.id === active?.id ? 'palette__item palette__item--active' : 'palette__item'
                  }
                  onMouseEnter={() => setActiveIndex(items.indexOf(item))}
                  onClick={() => run(item)}
                >
                  {item.label}
                  {item.hint ? <span className="palette__hint">{item.hint}</span> : null}
                </button>
              ))
            )}
          </div>
        )}
      </div>
    </div>
  );
}
