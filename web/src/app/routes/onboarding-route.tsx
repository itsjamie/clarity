import { useEffect, useState, type FormEvent } from 'react';
import { useNavigate } from 'react-router-dom';

import clarityLogoUrl from '@/assets/clarity-logo.svg';
import { useSessionState } from '@/hooks/use-session-state';
import { identityStore } from '@/lib/presence/presence-service';
import {
  defaultServerUrl,
  loadServerUrl,
  saveServerUrl,
} from '@/lib/settings/server-url';
import '@/styles/shell.css';

export function OnboardingRoute() {
  const navigate = useNavigate();
  const identity = useSessionState(identityStore);
  const [displayName, setDisplayName] = useState('');
  const [serverUrl, setServerUrl] = useState(loadServerUrl);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    void identityStore.load();
  }, []);

  useEffect(() => {
    if (identity.status === 'ready' && !busy) void navigate('/', { replace: true });
  }, [busy, identity.status, navigate]);

  const submit = async (event: FormEvent<HTMLFormElement>) => {
    event.preventDefault();
    setBusy(true);
    setError(null);
    try {
      saveServerUrl(serverUrl || defaultServerUrl());
      await identityStore.create(displayName.trim() || 'Anonymous');
      await navigate('/', { replace: true });
    } catch (caught) {
      setError(
        caught instanceof Error ? caught.message : 'The identity could not be created.',
      );
    } finally {
      setBusy(false);
    }
  };

  return (
    <div className="onboarding">
      <form className="onboarding__panel" onSubmit={(event) => void submit(event)}>
        <img className="onboarding__logo" src={clarityLogoUrl} alt="" />
        <p className="onboarding__eyebrow">First run</p>
        <h1>Nothing to sign up for.</h1>
        <p className="onboarding__lede">
          Clarity makes a key pair on this device and calls that your identity.
          Pick a name your friends will recognise — you can change it any time.
        </p>

        <label className="shell-field">
          <span>Display name</span>
          <input
            autoFocus
            type="text"
            placeholder="Jamie"
            value={displayName}
            onChange={(event) => setDisplayName(event.target.value)}
          />
        </label>

        <label className="shell-field">
          <span>Server</span>
          <input
            type="text"
            className="shell-field__mono"
            value={serverUrl}
            onChange={(event) => setServerUrl(event.target.value)}
          />
        </label>

        {identity.status === 'unsupported' ? (
          <p className="shell-error" role="alert">
            This browser cannot create an Ed25519 identity key. You can still
            join rooms from invite links.
          </p>
        ) : null}
        {error ? <p className="shell-error" role="alert">{error}</p> : null}

        <button
          type="submit"
          className="shell-button shell-button--accent shell-button--wide shell-button--tall"
          disabled={busy || identity.status === 'unsupported'}
        >
          {busy ? 'Creating identity…' : 'Create my identity'}
        </button>

        <div className="onboarding__notes">
          <OnboardingNote index="01">No account, no email, no password.</OnboardingNote>
          <OnboardingNote index="02">
            Screens and chat go peer to peer and are never stored.
          </OnboardingNote>
          <OnboardingNote index="03">
            You add friends by trading a short code.
          </OnboardingNote>
        </div>
      </form>
    </div>
  );
}

function OnboardingNote({ index, children }: { index: string; children: string }) {
  return (
    <span className="onboarding__note">
      <span aria-hidden="true">{index}</span>
      {children}
    </span>
  );
}
