import { useState, type FormEvent } from 'react';

import { useNow } from '@/hooks/use-now';
import { useSessionState } from '@/hooks/use-session-state';
import {
  contactsStore,
  dismissedRequestsStore,
  identityStore,
  presenceStore,
} from '@/lib/presence/presence-service';
import { INVITE_TTL_MS } from '@/lib/identity/contacts-store';
import { openRequests } from '@/lib/identity/dismissed-requests';
import { initials } from '@/features/shell/lib/friend-rows';

export function FriendsRoute() {
  const identity = useSessionState(identityStore);
  const contacts = useSessionState(contactsStore);
  const presence = useSessionState(presenceStore);
  const dismissed = useSessionState(dismissedRequestsStore);
  const now = useNow(30_000);
  const waiting = contacts.contacts.filter((contact) => !contact.confirmed);
  const invites = openRequests(presence.requests, contacts.contacts, dismissed.codes);

  const accept = (code: string) => {
    try {
      contactsStore.add(code, '', identity.friendCode);
    } catch {
      // Already a contact or a malformed code: nothing sensible to add, so
      // dismiss it rather than leaving a row whose Accept can never work.
      dismissedRequestsStore.dismiss(code);
    }
  };

  return (
    <div className="shell-page">
      <h1>Add a friend</h1>
      <p className="shell-page__lede">
        Clarity has no accounts. Your identity lives on this device as a key
        pair; a friend code is its public half. Trade codes once and you'll see
        each other's rooms from then on.
      </p>

      <div className="friends-grid">
        <section className="shell-panel">
          <div className="shell-panel__eyebrow">Your code</div>
          <div className="friends-code">{identity.friendCode ?? '…'}</div>
          <div className="friends-code__actions">
            <CopyCodeButton code={identity.friendCode} />
            <RotateButton />
          </div>
          <p className="shell-panel__note">
            Your code is your key pair's public half, so Rotate makes a new key
            pair and the old code stops working. Friends you've added stay on
            your list; people who added you will need your new code.
          </p>
        </section>

        <AddFriendPanel ownCode={identity.friendCode} />
      </div>

      {invites.length > 0 ? (
        <div className="friends-waiting">
          <div className="shell-panel__eyebrow">Invites for you</div>
          <div className="friends-waiting__list">
            {invites.map((code) => (
              <div key={code} className="friends-waiting__row">
                <span className="friend-row__avatar" aria-hidden="true">
                  {initials(code)}
                </span>
                <span className="friends-waiting__copy">
                  <strong>{code}</strong>
                  <span>added you and is waiting</span>
                </span>
                <button
                  type="button"
                  className="shell-button shell-button--accent"
                  onClick={() => accept(code)}
                >
                  Accept
                </button>
                <button
                  type="button"
                  className="shell-button shell-button--danger-ghost"
                  onClick={() => dismissedRequestsStore.dismiss(code)}
                >
                  Dismiss
                </button>
              </div>
            ))}
          </div>
        </div>
      ) : null}

      {waiting.length > 0 ? (
        <div className="friends-waiting">
          <div className="shell-panel__eyebrow">Waiting on them</div>
          <div className="friends-waiting__list">
            {waiting.map((contact) => (
              <div key={contact.code} className="friends-waiting__row">
                <span className="friend-row__avatar" aria-hidden="true">
                  {initials(contact.name || contact.code)}
                </span>
                <span className="friends-waiting__copy">
                  <strong>{contact.name || contact.code}</strong>
                  <span>{contact.code} · {expiresIn(contact.addedAt, now)}</span>
                </span>
                <button
                  type="button"
                  className="shell-button shell-button--danger-ghost"
                  onClick={() => contactsStore.remove(contact.code)}
                >
                  Cancel
                </button>
              </div>
            ))}
          </div>
        </div>
      ) : null}
    </div>
  );
}

function CopyCodeButton({ code }: { code: string | null }) {
  const [copied, setCopied] = useState(false);
  return (
    <button
      type="button"
      className="shell-button shell-button--ghost"
      disabled={!code}
      onClick={() => {
        if (!code) return;
        void navigator.clipboard.writeText(code).then(() => {
          setCopied(true);
          window.setTimeout(() => setCopied(false), 1_500);
        });
      }}
    >
      {copied ? 'Copied' : 'Copy code'}
    </button>
  );
}

function RotateButton() {
  const [busy, setBusy] = useState(false);
  return (
    <button
      type="button"
      className="shell-button shell-button--ghost"
      disabled={busy}
      onClick={() => {
        if (!window.confirm('Rotate your friend code? This replaces your key pair and the old code stops working.')) return;
        setBusy(true);
        void identityStore.rotate().finally(() => setBusy(false));
      }}
    >
      Rotate
    </button>
  );
}

function AddFriendPanel({ ownCode }: { ownCode: string | null }) {
  const [code, setCode] = useState('');
  const [name, setName] = useState('');
  const [error, setError] = useState<string | null>(null);

  const submit = (event: FormEvent<HTMLFormElement>) => {
    event.preventDefault();
    try {
      contactsStore.add(code, name, ownCode);
      setCode('');
      setName('');
      setError(null);
    } catch (caught) {
      setError(caught instanceof Error ? caught.message : 'The friend could not be added.');
    }
  };

  return (
    <section className="shell-panel">
      <form onSubmit={submit}>
        <div className="shell-panel__eyebrow">Their code</div>
        <input
          className="friends-code-input"
          type="text"
          placeholder="clr-XXXX-XXXX"
          aria-label="Friend code"
          value={code}
          onChange={(event) => {
            setCode(event.target.value);
            setError(null);
          }}
        />
        <label className="shell-field">
          <span>Name them (only you see this)</span>
          <input
            type="text"
            placeholder="e.g. Mara"
            value={name}
            onChange={(event) => setName(event.target.value)}
          />
        </label>
        {error ? <p className="shell-error" role="alert">{error}</p> : null}
        <button type="submit" className="shell-button shell-button--accent shell-button--wide">
          Add friend
        </button>
      </form>
    </section>
  );
}

/**
 * The invite's remaining life, so the row explains its own later
 * disappearance. Invites are answered in the moment or age out
 * ([`INVITE_TTL_MS`]).
 */
function expiresIn(addedAt: number, now = Date.now()): string {
  const remaining = addedAt + INVITE_TTL_MS - now;
  if (remaining <= 60_000) return 'expires any moment';
  return `expires in ${Math.round(remaining / 60_000)}m`;
}
