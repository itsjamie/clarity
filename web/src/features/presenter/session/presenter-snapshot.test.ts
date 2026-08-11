import type { PeerSnapshot, RoomSnapshot } from '@/generated/protocol';
import { withPendingViewer } from './presenter-snapshot';

describe('presenter room events', () => {
  it('adds a pending viewer from the incremental viewer event exactly once', () => {
    const snapshot: RoomSnapshot = {
      roomId: 'room',
      lifecycle: 'open',
      sharingState: 'idle',
      accessPolicy: 'approvalRequired',
      maximumViewers: 4,
      expiresAt: '2026-01-01T00:00:00Z',
      expiresInSeconds: 3_600,
      presenterConnected: true,
      pendingViewers: [],
      approvedViewers: [],
    };
    const viewer: PeerSnapshot = {
      peerId: 'viewer',
      displayName: 'Ada',
      role: 'viewer',
      viewerState: 'pending',
      connected: true,
      joinedAt: '2026-01-01T00:00:00Z',
      friendCode: null,
    };
    const first = withPendingViewer(snapshot, viewer);
    const duplicate = withPendingViewer(first, viewer);
    expect(first.pendingViewers).toEqual([viewer]);
    expect(duplicate).toBe(first);
  });
});
