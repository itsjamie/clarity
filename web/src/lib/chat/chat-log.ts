import type { ExternalStateStore } from '@/hooks/use-session-state';

export type ChatEntry =
  | { kind: 'message'; id: number; sender: string; text: string; at: number; self: boolean }
  | { kind: 'system'; id: number; text: string; at: number };

export interface ChatLogState {
  entries: readonly ChatEntry[];
}

const MAXIMUM_ENTRIES = 200;

/**
 * The in-memory, ephemeral chat transcript for one room session. Messages
 * only ever live here; nothing is stored server-side.
 */
export class ChatLog implements ExternalStateStore<ChatLogState> {
  readonly #listeners = new Set<() => void>();
  #nextId = 1;
  #state: ChatLogState = { entries: [] };

  public getSnapshot = (): ChatLogState => this.#state;

  public subscribe = (listener: () => void): (() => void) => {
    this.#listeners.add(listener);
    return () => this.#listeners.delete(listener);
  };

  public addMessage(sender: string, text: string, self = false): void {
    this.#append({
      kind: 'message',
      id: this.#nextId++,
      sender,
      text,
      at: Date.now(),
      self,
    });
  }

  public addSystem(text: string): void {
    const last = this.#state.entries.at(-1);
    if (last?.kind === 'system' && last.text === text) return;
    this.#append({ kind: 'system', id: this.#nextId++, text, at: Date.now() });
  }

  #append(entry: ChatEntry): void {
    const entries = [...this.#state.entries, entry];
    if (entries.length > MAXIMUM_ENTRIES) entries.splice(0, entries.length - MAXIMUM_ENTRIES);
    this.#state = { entries };
    this.#listeners.forEach((listener) => listener());
  }
}
