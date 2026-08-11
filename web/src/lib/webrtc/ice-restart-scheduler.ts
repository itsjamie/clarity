export interface IceRestartSchedulerOptions {
  /** Asks the presenter for an ICE restart (`signal:ice-restart`). */
  requestRestart: () => void;
  /** Debounce before reacting to `disconnected`, which often self-heals. */
  disconnectedDelayMs?: number;
  /** Minimum spacing between automatic restart requests. */
  minimumIntervalMs?: number;
  now?: () => number;
  setTimer?: (callback: () => void, delayMs: number) => number;
  clearTimer?: (timer: number) => void;
}

export const DISCONNECTED_RESTART_DELAY_MS = 3_000;
export const MINIMUM_RESTART_INTERVAL_MS = 5_000;

/**
 * Decides when a viewer should ask the presenter for an ICE restart.
 * `failed` restarts immediately, `disconnected` after a debounce, and
 * automatic requests are rate limited; a manual request bypasses the limit.
 */
export class IceRestartScheduler {
  readonly #requestRestart: () => void;
  readonly #disconnectedDelayMs: number;
  readonly #minimumIntervalMs: number;
  readonly #now: () => number;
  readonly #setTimer: (callback: () => void, delayMs: number) => number;
  readonly #clearTimer: (timer: number) => void;
  #timer: number | null = null;
  #lastRequestAt: number | null = null;

  public constructor(options: IceRestartSchedulerOptions) {
    this.#requestRestart = options.requestRestart;
    this.#disconnectedDelayMs = options.disconnectedDelayMs ?? DISCONNECTED_RESTART_DELAY_MS;
    this.#minimumIntervalMs = options.minimumIntervalMs ?? MINIMUM_RESTART_INTERVAL_MS;
    this.#now = options.now ?? (() => performance.now());
    this.#setTimer = options.setTimer ?? ((callback, delay) => window.setTimeout(callback, delay));
    this.#clearTimer = options.clearTimer ?? ((timer) => window.clearTimeout(timer));
  }

  /** Reacts to a connection state change on the media peer connection. */
  public update(connectionState: RTCPeerConnectionState | 'new'): void {
    switch (connectionState) {
      case 'failed':
        this.#schedule(0);
        break;
      case 'disconnected':
        this.#schedule(this.#disconnectedDelayMs);
        break;
      case 'connected':
      case 'closed':
        this.#cancel();
        break;
      default:
        break;
    }
  }

  /** A user-initiated restart request; bypasses the rate limit. */
  public requestNow(): void {
    this.#cancel();
    this.#lastRequestAt = this.#now();
    this.#requestRestart();
  }

  public stop(): void {
    this.#cancel();
  }

  #schedule(delayMs: number): void {
    if (this.#timer !== null) return;
    const delay = Math.max(delayMs, this.#remainingCooldown());
    this.#timer = this.#setTimer(() => {
      this.#timer = null;
      this.#lastRequestAt = this.#now();
      this.#requestRestart();
    }, delay);
  }

  #remainingCooldown(): number {
    if (this.#lastRequestAt === null) return 0;
    return Math.max(0, this.#minimumIntervalMs - (this.#now() - this.#lastRequestAt));
  }

  #cancel(): void {
    if (this.#timer !== null) this.#clearTimer(this.#timer);
    this.#timer = null;
  }
}
