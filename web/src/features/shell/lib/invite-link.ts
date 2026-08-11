export interface InviteTarget {
  /** The full invite URL as entered. */
  url: string;
  /** True when the invite points at this deployment. */
  sameOrigin: boolean;
  /** Router path (path + query + fragment) for a same-origin invite. */
  appPath: string;
  roomId: string;
}

/**
 * Parses a pasted viewer invitation (`https://host/r/<roomId>?access=…#secret`).
 * Returns `null` for anything that is not a Clarity invite link.
 */
export function parseInviteLink(
  input: string,
  origin: string = window.location.origin,
): InviteTarget | null {
  let url: URL;
  try {
    url = new URL(input.trim());
  } catch {
    return null;
  }
  if (url.protocol !== 'https:' && url.protocol !== 'http:') return null;
  const match = /^\/r\/([A-Za-z0-9_-]+)$/.exec(url.pathname);
  if (!match) return null;
  return {
    url: url.toString(),
    sameOrigin: url.origin === origin,
    appPath: `${url.pathname}${url.search}${url.hash}`,
    roomId: match[1]!,
  };
}
