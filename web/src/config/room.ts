export const MINIMUM_VIEWERS = 1;
export const MAXIMUM_VIEWERS = 10;
export const PUBLIC_ROOM_VIEWERS = 10;
export const DEFAULT_APPROVAL_VIEWERS = 4;

export function isValidViewerLimit(value: number): boolean {
  return Number.isInteger(value) && value >= MINIMUM_VIEWERS && value <= MAXIMUM_VIEWERS;
}
