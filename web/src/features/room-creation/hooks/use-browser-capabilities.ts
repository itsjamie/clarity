import { useMemo } from 'react';

export interface BrowserCapabilities {
  canPresent: boolean;
  canView: boolean;
  reason?: string;
}

export function useBrowserCapabilities(): BrowserCapabilities {
  return useMemo(() => {
    const canView = typeof RTCPeerConnection !== 'undefined';
    const mobile = /Android|iPhone|iPad|iPod/iu.test(navigator.userAgent);
    if (!window.isSecureContext) {
      return { canPresent: false, canView, reason: 'Presentation requires HTTPS or localhost.' };
    }
    if (mobile) {
      return { canPresent: false, canView, reason: 'Mobile browsers can view, but cannot present.' };
    }
    if (!navigator.mediaDevices?.getDisplayMedia || typeof RTCPeerConnection === 'undefined') {
      return { canPresent: false, canView, reason: 'This browser does not expose the required capture APIs.' };
    }
    return { canPresent: true, canView: true };
  }, []);
}
