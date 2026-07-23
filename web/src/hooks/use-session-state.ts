import { useSyncExternalStore } from 'react';

export interface ExternalStateStore<T> {
  subscribe(listener: () => void): () => void;
  getSnapshot(): T;
}

export function useSessionState<T>(store: ExternalStateStore<T>): T {
  return useSyncExternalStore(
    (listener) => store.subscribe(listener),
    () => store.getSnapshot(),
    () => store.getSnapshot(),
  );
}
