import { QualityAdaptationController } from './quality-adaptation';

describe('quality adaptation', () => {
  it('does not react to one bad sample and degrades one profile after three', () => {
    const controller = new QualityAdaptationController('text');
    expect(controller.evaluate({ packetLossRatio: 0.1 }, 0).changed).toBe(false);
    expect(controller.evaluate({ packetLossRatio: 0.1 }, 2_000).changed).toBe(false);
    const decision = controller.evaluate({ packetLossRatio: 0.1 }, 10_000);
    expect(decision.changed).toBe(true);
    expect(decision.profile.id).toBe('text-medium');
  });

  it('requires sustained health and an upgrade cooldown', () => {
    const controller = new QualityAdaptationController('motion', 'adaptive', 1);
    let changed = false;
    for (let index = 0; index < 15; index += 1) {
      changed = controller.evaluate(
        { packetLossRatio: 0, roundTripTimeMs: 30, framesPerSecond: 60, bitrate: 10_000_000, availableOutgoingBitrate: 30_000_000, qualityLimitationReason: 'none' },
        30_000 + index * 2_000,
      ).changed;
    }
    expect(changed).toBe(true);
    expect(controller.profile.id).toBe('motion-high');
  });

  it('never automatically changes a fixed profile', () => {
    const controller = new QualityAdaptationController('text', 'fixed');
    for (let index = 0; index < 10; index += 1) {
      expect(controller.evaluate({ qualityLimitationReason: 'cpu' }, index * 20_000).changed).toBe(false);
    }
    expect(controller.profile.id).toBe('text-high');
  });

  it('keeps high quality for static content when no pressure signal is present', () => {
    const controller = new QualityAdaptationController('text');
    for (let index = 0; index < 6; index += 1) {
      const decision = controller.evaluate(
        {
          packetLossRatio: 0,
          roundTripTimeMs: 20,
          framesPerSecond: 0,
          bitrate: 0,
          availableOutgoingBitrate: 30_000_000,
          qualityLimitationReason: 'none',
        },
        index * 10_000,
      );
      expect(decision.changed).toBe(false);
      expect(decision.profile.id).toBe('text-high');
    }
  });
});
