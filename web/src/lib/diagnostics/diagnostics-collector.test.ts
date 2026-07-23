import { DiagnosticsCollector } from './diagnostics-collector';

describe('sanitized diagnostics export', () => {
  it('excludes secrets, signaling bodies, fragments, and IP addresses', () => {
    const collector = new DiagnosticsCollector();
    collector.record('signal', {
      resumeToken: 'token',
      sdp: 'raw',
      candidate: 'candidate:1 1 UDP 1 192.0.2.1',
      page: 'https://example.test/r/room#viewer-secret',
    });
    const json = JSON.stringify(collector.export({ address: '2001:db8::1' }));
    expect(json).not.toContain('viewer-secret');
    expect(json).not.toContain('192.0.2.1');
    expect(json).not.toContain('2001:db8::1');
    expect(json).not.toContain('raw');
    expect(json).not.toContain('token');
  });
});
