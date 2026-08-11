import type { FriendPresence, HostedRoom } from '@/generated/protocol';
import type { Contact } from '@/lib/identity/contacts-store';

export interface FriendRow {
  code: string;
  name: string;
  online: boolean;
  confirmed: boolean;
  lastSeenSecondsAgo: number | null;
  hosting: HostedRoom | null;
}

/**
 * Joins the local contact list with live presence, one row per contact.
 * Online friends sort first, then most recently seen.
 */
export function friendRows(
  contacts: readonly Contact[],
  friends: readonly FriendPresence[],
): FriendRow[] {
  const presence = new Map(friends.map((friend) => [friend.code, friend]));
  return contacts
    .map((contact) => {
      const seen = presence.get(contact.code);
      return {
        code: contact.code,
        name: contact.name || contact.code,
        online: seen?.online ?? false,
        confirmed: contact.confirmed,
        lastSeenSecondsAgo: seen?.lastSeenSecondsAgo ?? null,
        hosting: seen?.hosting ?? null,
      };
    })
    .sort((a, b) => {
      if (a.online !== b.online) return a.online ? -1 : 1;
      const aSeen = a.lastSeenSecondsAgo ?? Number.POSITIVE_INFINITY;
      const bSeen = b.lastSeenSecondsAgo ?? Number.POSITIVE_INFINITY;
      if (aSeen !== bSeen) return aSeen - bSeen;
      return a.name.localeCompare(b.name);
    });
}

/** Rooms friends are hosting right now: live first, then paused, then idle. */
export function liveRooms(rows: readonly FriendRow[]): FriendRow[] {
  const rank = (row: FriendRow): number => {
    switch (row.hosting!.sharingState) {
      case 'live':
        return 2;
      case 'paused':
        return 1;
      case 'idle':
        return 0;
    }
  };
  return rows
    .filter((row) => row.hosting !== null)
    .sort((a, b) => rank(b) - rank(a));
}

/** Compact "last seen" label: `now` while online, else `5m`, `3h`, `2d`. */
export function formatLastSeen(row: Pick<FriendRow, 'online' | 'lastSeenSecondsAgo'>): string {
  if (row.online) return 'now';
  const seconds = row.lastSeenSecondsAgo;
  if (seconds === null) return 'away';
  if (seconds < 60) return 'now';
  if (seconds < 3_600) return `${Math.floor(seconds / 60)}m`;
  if (seconds < 86_400) return `${Math.floor(seconds / 3_600)}h`;
  return `${Math.floor(seconds / 86_400)}d`;
}

/** Two-letter avatar initials from a display name or friend code. */
export function initials(name: string): string {
  const trimmed = name.trim();
  if (trimmed.toLowerCase().startsWith('clr-')) {
    return trimmed.slice(4, 6).toUpperCase();
  }
  const words = trimmed.split(/\s+/).filter(Boolean);
  if (words.length === 0) return '??';
  if (words.length === 1) return words[0]!.slice(0, 2).toUpperCase();
  return `${words[0]![0]}${words[1]![0]}`.toUpperCase();
}
