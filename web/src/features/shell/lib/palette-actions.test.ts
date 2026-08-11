import type { FriendRow } from './friend-rows';
import { ROOM_ITEM_PREFIX, paletteItems } from './palette-actions';

function hostingRow(name: string, sharingState: 'idle' | 'live' | 'paused'): FriendRow {
  return {
    code: `clr-${name.toUpperCase()}1-AAAA`,
    name,
    online: true,
    confirmed: true,
    lastSeenSecondsAgo: 0,
    hosting: {
      roomId: 'room-1',
      viewerUrl: 'https://example.test/r/room-1',
      viewerCount: 2,
      sharingState,
    },
  };
}

describe('paletteItems', () => {
  it('lists every action when the query is empty', () => {
    const items = paletteItems('', [], false);
    expect(items.map((item) => item.id)).toEqual([
      'create-room',
      'add-friend',
      'open-settings',
      'join-link',
    ]);
  });

  it('offers the theatre toggle only inside a room', () => {
    expect(paletteItems('', [], true).map((item) => item.id)).toContain('toggle-theatre');
    expect(paletteItems('', [], false).map((item) => item.id)).not.toContain('toggle-theatre');
  });

  it('adds a join row per hosted room with its sharing state as the hint', () => {
    const items = paletteItems('', [hostingRow('Mara', 'live'), hostingRow('Dan', 'idle')], false);
    const rooms = items.filter((item) => item.id.startsWith(ROOM_ITEM_PREFIX));
    expect(rooms.map((item) => item.label)).toEqual(["Join Mara's room", "Join Dan's room"]);
    expect(rooms.map((item) => item.hint)).toEqual(['live', 'idle']);
  });

  it('filters by a case-insensitive substring on the label', () => {
    const items = paletteItems('mara', [hostingRow('Mara', 'live')], false);
    expect(items.map((item) => item.label)).toEqual(["Join Mara's room"]);
    expect(paletteItems('SETTING', [], false).map((item) => item.id)).toEqual(['open-settings']);
    expect(paletteItems('no such thing', [], false)).toEqual([]);
  });
});
