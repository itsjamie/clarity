import { IceRestartScheduler } from './ice-restart-scheduler';

interface PendingTimer {
  callback: () => void;
  dueAt: number;
}

class FakeClock {
  #now = 0;
  #nextTimer = 1;
  readonly #timers = new Map<number, PendingTimer>();

  readonly now = (): number => this.#now;

  readonly setTimer = (callback: () => void, delayMs: number): number => {
    const id = this.#nextTimer++;
    this.#timers.set(id, { callback, dueAt: this.#now + delayMs });
    return id;
  };

  readonly clearTimer = (timer: number): void => {
    this.#timers.delete(timer);
  };

  public advance(milliseconds: number): void {
    const target = this.#now + milliseconds;
    for (;;) {
      const due = [...this.#timers.entries()]
        .filter(([, timer]) => timer.dueAt <= target)
        .sort((a, b) => a[1].dueAt - b[1].dueAt)[0];
      if (!due) break;
      this.#timers.delete(due[0]);
      this.#now = due[1].dueAt;
      due[1].callback();
    }
    this.#now = target;
  }
}

function createScheduler(clock: FakeClock) {
  const requestRestart = vi.fn();
  const scheduler = new IceRestartScheduler({
    requestRestart,
    now: clock.now,
    setTimer: clock.setTimer,
    clearTimer: clock.clearTimer,
  });
  return { scheduler, requestRestart };
}

describe('ice restart scheduler', () => {
  it('requests immediately on failed', () => {
    const clock = new FakeClock();
    const { scheduler, requestRestart } = createScheduler(clock);

    scheduler.update('failed');
    clock.advance(0);

    expect(requestRestart).toHaveBeenCalledTimes(1);
  });

  it('debounces disconnected and cancels when the connection recovers', () => {
    const clock = new FakeClock();
    const { scheduler, requestRestart } = createScheduler(clock);

    scheduler.update('disconnected');
    clock.advance(2_000);
    scheduler.update('connected');
    clock.advance(10_000);

    expect(requestRestart).not.toHaveBeenCalled();
  });

  it('requests after the disconnected debounce elapses', () => {
    const clock = new FakeClock();
    const { scheduler, requestRestart } = createScheduler(clock);

    scheduler.update('disconnected');
    clock.advance(2_999);
    expect(requestRestart).not.toHaveBeenCalled();
    clock.advance(1);
    expect(requestRestart).toHaveBeenCalledTimes(1);
  });

  it('does not double-schedule while a request is pending', () => {
    const clock = new FakeClock();
    const { scheduler, requestRestart } = createScheduler(clock);

    scheduler.update('disconnected');
    scheduler.update('disconnected');
    scheduler.update('failed');
    clock.advance(3_000);

    expect(requestRestart).toHaveBeenCalledTimes(1);
  });

  it('rate limits automatic requests to the minimum interval', () => {
    const clock = new FakeClock();
    const { scheduler, requestRestart } = createScheduler(clock);

    scheduler.update('failed');
    clock.advance(0);
    expect(requestRestart).toHaveBeenCalledTimes(1);

    scheduler.update('failed');
    clock.advance(4_999);
    expect(requestRestart).toHaveBeenCalledTimes(1);
    clock.advance(1);
    expect(requestRestart).toHaveBeenCalledTimes(2);
  });

  it('lets a manual request bypass the rate limit', () => {
    const clock = new FakeClock();
    const { scheduler, requestRestart } = createScheduler(clock);

    scheduler.update('failed');
    clock.advance(0);
    scheduler.requestNow();

    expect(requestRestart).toHaveBeenCalledTimes(2);
  });

  it('starts the cooldown from a manual request', () => {
    const clock = new FakeClock();
    const { scheduler, requestRestart } = createScheduler(clock);

    scheduler.requestNow();
    scheduler.update('failed');
    clock.advance(4_999);
    expect(requestRestart).toHaveBeenCalledTimes(1);
    clock.advance(1);
    expect(requestRestart).toHaveBeenCalledTimes(2);
  });

  it('stops pending work', () => {
    const clock = new FakeClock();
    const { scheduler, requestRestart } = createScheduler(clock);

    scheduler.update('disconnected');
    scheduler.stop();
    clock.advance(60_000);

    expect(requestRestart).not.toHaveBeenCalled();
  });
});
