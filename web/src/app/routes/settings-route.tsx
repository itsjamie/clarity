import { useState } from 'react';

import { useSessionState } from '@/hooks/use-session-state';
import { identityStore, presenceStore } from '@/lib/presence/presence-service';
import type { PresenceStatus } from '@/lib/presence/presence-client';
import type { IdentityStatus } from '@/lib/identity/identity-store';
import {
  loadAppSettings,
  saveAppSettings,
  type AppSettings,
} from '@/lib/settings/app-settings';
import {
  defaultServerUrl,
  loadServerUrl,
  saveServerUrl,
} from '@/lib/settings/server-url';
import type { CaptureMode } from '@/lib/webrtc/profiles';
import type { CaptureResolution } from '@/lib/media/capture-resolution';

export function SettingsRoute() {
  const identity = useSessionState(identityStore);
  const presence = useSessionState(presenceStore);
  const [serverUrl, setServerUrl] = useState(loadServerUrl);
  const [settings, setSettings] = useState<AppSettings>(loadAppSettings);
  const update = (patch: Partial<AppSettings>) => setSettings(saveAppSettings(patch));
  const connection = connectionSummary(presence.status, identity.status);

  return (
    <div className="shell-page">
      <h1>Settings</h1>

      <div className="settings-stack">
        <section className="shell-panel">
          <div className="shell-panel__eyebrow">Identity</div>
          <div className="settings-fields">
            <label className="shell-field">
              <span>Display name</span>
              <input
                type="text"
                value={identity.displayName}
                onChange={(event) => identityStore.setDisplayName(event.target.value)}
              />
            </label>
            <label className="shell-field">
              <span>This device</span>
              <input
                type="text"
                value={identity.deviceLabel}
                onChange={(event) => identityStore.setDeviceLabel(event.target.value)}
              />
            </label>
          </div>
          <p className="shell-panel__note">
            Your key never leaves this browser. Clearing site data deletes the
            identity — friends will need your new code.
            {identity.friendCode ? ` Your code is ${identity.friendCode}.` : ''}
          </p>
        </section>

        <section className="shell-panel">
          <div className="shell-panel__eyebrow">Capture defaults</div>
          <div className="settings-fields">
            <label className="shell-field">
              <span>Profile</span>
              <select
                value={settings.captureMode}
                onChange={(event) => update({ captureMode: event.target.value as CaptureMode })}
              >
                <option value="text">Text · 30 fps</option>
                <option value="motion">Motion · 60 fps</option>
              </select>
            </label>
            <label className="shell-field">
              <span>Max capture</span>
              <select
                value={settings.captureResolution}
                onChange={(event) =>
                  update({ captureResolution: event.target.value as CaptureResolution })
                }
              >
                <option value="1440p">2560 × 1440</option>
                <option value="4k">3840 × 2160</option>
              </select>
            </label>
          </div>
          <label className="settings-check">
            <input
              type="checkbox"
              checked={settings.captureAudio}
              onChange={(event) => update({ captureAudio: event.target.checked })}
            />
            <span>
              <span>Include system audio when the source allows it</span>
              <span className="settings-check__note">
                Clarity never asks for your microphone or camera.
              </span>
            </span>
          </label>
          <p className="shell-panel__note">
            New rooms start from these defaults; each room can still change them
            while sharing.
          </p>
        </section>

        <section className="shell-panel">
          <div className="shell-panel__eyebrow">Network</div>
          <label className="shell-field">
            <span>Signaling server</span>
            <input
              type="text"
              className="shell-field__mono"
              value={serverUrl}
              onChange={(event) => {
                setServerUrl(event.target.value);
                saveServerUrl(event.target.value);
              }}
            />
          </label>
          <div className={`settings-connection settings-connection--${connection.tone}`} role="status">
            <i aria-hidden="true" />
            {connection.label}
          </div>
          <label className="settings-check">
            <input
              type="checkbox"
              checked={settings.forceRelay}
              onChange={(event) => update({ forceRelay: event.target.checked })}
            />
            <span>
              <span>Always relay through my server</span>
              <span className="settings-check__note">
                Hides your IP from peers. Adds latency and uses the server's
                bandwidth. Applies to rooms you join or open from now on.
              </span>
            </span>
          </label>
          <p className="shell-panel__note">
            The web app always talks to the server that serves it
            ({defaultServerUrl()}). To use another deployment, open Clarity there.
          </p>
        </section>
      </div>
    </div>
  );
}

function connectionSummary(
  presence: PresenceStatus,
  identity: IdentityStatus,
): { tone: 'ok' | 'wait' | 'warn'; label: string } {
  if (identity === 'unsupported') {
    return {
      tone: 'warn',
      label: 'This browser cannot hold an identity key, so presence is off. Rooms still work.',
    };
  }
  switch (presence) {
    case 'ready':
      return { tone: 'ok', label: 'Connected · presence is live' };
    case 'connecting':
    case 'authenticating':
      return { tone: 'wait', label: 'Connecting…' };
    case 'reconnecting':
      return { tone: 'wait', label: 'Connection lost · reconnecting…' };
    case 'failed':
      return { tone: 'warn', label: 'The server rejected the presence connection.' };
    case 'closed':
      return { tone: 'warn', label: 'Not connected.' };
    case 'idle':
      return { tone: 'wait', label: 'Waiting for your identity to load…' };
  }
}
