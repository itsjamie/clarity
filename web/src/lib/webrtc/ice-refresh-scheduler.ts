export interface IceRefreshSchedulerOptions {
  /** Sends `ice:refresh` so the server issues fresh TURN credentials. */
  requestRefresh: () => void;
  /** How long before the credential expiry the refresh is requested. */
  leadMs?: number;
  /** Retry cadence when the expiry is unreadable or already close. */
  retryMs?: number;
  now?: () => number;
  setTimer?: (callback: () => void, delayMs: number) => number;
  clearTimer?: (timer: number) => void;
}

export const ICE_REFRESH_LEAD_MS = 60_000;
export const ICE_REFRESH_RETRY_MS = 60_000;
const MINIMUM_DELAY_MS = 5_000;

/**
 * Schedules `ice:refresh` ahead of the ICE configuration's TURN credential
 * expiry, so an ICE restart or a rebuilt connection never gathers relay
 * candidates with expired credentials. Call `schedule` with every
 * configuration the server issues (`auth:succeeded`, `ice:configuration`);
 * the refreshed configuration re-arms the timer from its own expiry, and an
 * unanswered refresh retries on a fixed cadence.
 */
export class IceRefreshScheduler {
  readonly #requestRefresh: () => void;
  readonly #leadMs: number;
  readonly #retryMs: number;
  readonly #now: () => number;
  readonly #setTimer: (callback: () => void, delayMs: number) => number;
  readonly #clearTimer: (timer: number) => void;
  #timer: number | null = null;

  public constructor(options: IceRefreshSchedulerOptions) {
    this.#requestRefresh = options.requestRefresh;
    this.#leadMs = options.leadMs ?? ICE_REFRESH_LEAD_MS;
    this.#retryMs = options.retryMs ?? ICE_REFRESH_RETRY_MS;
    this.#now = options.now ?? (() => Date.now());
    this.#setTimer = options.setTimer ?? ((callback, delay) => window.setTimeout(callback, delay));
    this.#clearTimer = options.clearTimer ?? ((timer) => window.clearTimeout(timer));
  }

  /** Arms the timer for a configuration expiring at `expiresAt` (RFC 3339). */
  public schedule(expiresAt: string): void {
    this.stop();
    const expiryMs = Date.parse(expiresAt);
    const delay = Number.isNaN(expiryMs)
      ? this.#retryMs
      : Math.max(expiryMs - this.#now() - this.#leadMs, MINIMUM_DELAY_MS);
    this.#arm(delay);
  }

  public stop(): void {
    if (this.#timer !== null) this.#clearTimer(this.#timer);
    this.#timer = null;
  }

  #arm(delayMs: number): void {
    this.#timer = this.#setTimer(() => {
      this.#timer = null;
      this.#requestRefresh();
      // Retry until a fresh configuration re-arms via schedule().
      this.#arm(this.#retryMs);
    }, delayMs);
  }
}
