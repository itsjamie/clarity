import type { PeerSnapshot, RoomSnapshot } from '@/generated/protocol';

export function withPendingViewer(
  snapshot: RoomSnapshot,
  viewer: PeerSnapshot,
): RoomSnapshot {
  if (snapshot.pendingViewers.some((candidate) => candidate.peerId === viewer.peerId)) {
    return snapshot;
  }
  return {
    ...snapshot,
    pendingViewers: [...snapshot.pendingViewers, viewer],
  };
}

export function withResumedViewer(snapshot: RoomSnapshot, peerId: string): RoomSnapshot {
  return {
    ...snapshot,
    approvedViewers: snapshot.approvedViewers.map((viewer) =>
      viewer.peerId === peerId
        ? { ...viewer, connected: true, viewerState: 'approved' }
        : viewer,
    ),
  };
}
