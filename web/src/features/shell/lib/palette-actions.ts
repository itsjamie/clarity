import type { FriendRow } from './friend-rows';

export interface PaletteItem {
  id: string;
  label: string;
  /** Right-aligned mono hint: a shortcut ('⌘N') or a room state ('live'). */
  hint: string | null;
}

export const ROOM_ITEM_PREFIX = 'room:';

/**
 * The command list for the palette: fixed actions plus one "Join …" row per
 * room a friend is hosting right now, filtered by a case-insensitive substring
 * match on the label. An empty query returns everything.
 */
export function paletteItems(
  query: string,
  rooms: readonly FriendRow[],
  inRoom: boolean,
): PaletteItem[] {
  const items: PaletteItem[] = [
    { id: 'create-room', label: 'Create room', hint: '⌘N' },
  ];
  for (const row of rooms) {
    if (!row.hosting) continue;
    items.push({
      id: `${ROOM_ITEM_PREFIX}${row.code}`,
      label: `Join ${row.name}'s room`,
      // Matches the desktop palette: live, paused, and idle are distinct.
      hint: row.hosting.sharingState,
    });
  }
  items.push(
    { id: 'add-friend', label: 'Add a friend', hint: '⌘⇧A' },
    { id: 'open-settings', label: 'Open settings', hint: '⌘,' },
    { id: 'join-link', label: 'Join by link', hint: null },
  );
  if (inRoom) {
    items.push({ id: 'toggle-theatre', label: 'Toggle theatre mode', hint: 'T' });
  }
  const needle = query.trim().toLowerCase();
  if (!needle) return items;
  return items.filter((item) => item.label.toLowerCase().includes(needle));
}
