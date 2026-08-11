import { parseInviteLink } from './invite-link';

describe('invite link parsing', () => {
  it('accepts a viewer invitation and keeps query and fragment', () => {
    const invite = parseInviteLink(
      ' https://example.test/r/AB12_cd?access=friends#secret ',
      'https://example.test',
    );
    expect(invite).toMatchObject({
      roomId: 'AB12_cd',
      sameOrigin: true,
      appPath: '/r/AB12_cd?access=friends#secret',
    });
  });

  it('marks other deployments as cross-origin', () => {
    const invite = parseInviteLink('https://other.test/r/room#s', 'https://example.test');
    expect(invite?.sameOrigin).toBe(false);
  });

  it('rejects non-invite input', () => {
    expect(parseInviteLink('not a link', 'https://example.test')).toBeNull();
    expect(parseInviteLink('https://example.test/present/room', 'https://example.test')).toBeNull();
    expect(parseInviteLink('https://example.test/r/', 'https://example.test')).toBeNull();
    expect(parseInviteLink('ftp://example.test/r/room', 'https://example.test')).toBeNull();
  });
});
