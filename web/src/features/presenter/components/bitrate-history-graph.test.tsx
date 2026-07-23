import { render, screen } from '@testing-library/react';

import { BitrateHistoryGraph } from './bitrate-history-graph';

describe('BitrateHistoryGraph', () => {
  it('exposes the current and peak bitrate to assistive technology', () => {
    const { container } = render(
      <BitrateHistoryGraph
        currentBitrate={12_000_000}
        samples={[
          { bitrate: 8_000_000, sampledAt: 1_000 },
          { bitrate: 15_000_000, sampledAt: 16_000 },
          { bitrate: 12_000_000, sampledAt: 31_000 },
        ]}
      />,
    );

    expect(screen.getByRole('img', {
      name: 'Upload bitrate over the last 30 seconds. Current 12.0 Mbps; peak 15.0 Mbps.',
    })).toBeVisible();
    expect(container.querySelector('.bitrate-chart__line')).toHaveAttribute('points');
    expect(screen.getByText('30s ago')).toBeVisible();
    expect(screen.getByText('Now')).toBeVisible();
  });
});
