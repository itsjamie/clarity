import type { ApiError, CreateRoomResponse, RoomAccessPolicy } from '@/generated/protocol';

export interface CreateRoomInput {
  maximumViewers: number;
  expiresInSeconds: number;
  accessPolicy: RoomAccessPolicy;
}

export async function createRoom(input: CreateRoomInput): Promise<CreateRoomResponse> {
  const response = await fetch('/api/v1/rooms', {
    method: 'POST',
    headers: { 'content-type': 'application/json' },
    credentials: 'same-origin',
    cache: 'no-store',
    body: JSON.stringify(input),
  });
  if (!response.ok) {
    const fallback = 'The room could not be created.';
    try {
      const error = (await response.json()) as ApiError;
      throw new Error(error.message || fallback);
    } catch (error) {
      if (error instanceof Error && error.message !== fallback) throw error;
      throw new Error(fallback);
    }
  }
  return (await response.json()) as CreateRoomResponse;
}
