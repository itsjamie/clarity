import { useEffect, useRef, useState, type FormEvent } from 'react';

import { useSessionState } from '@/hooks/use-session-state';
import { CHAT_MAX_TEXT_CHARACTERS } from '@/lib/chat/chat-channel';
import type { ChatEntry, ChatLog } from '@/lib/chat/chat-log';

interface ChatPanelProps {
  log: ChatLog;
  onSend: (text: string) => void;
  disabled?: boolean;
  compact?: boolean;
}

export function ChatPanel({ log, onSend, disabled = false, compact = false }: ChatPanelProps) {
  const { entries } = useSessionState(log);
  const [draft, setDraft] = useState('');
  const scrollRef = useRef<HTMLDivElement>(null);

  useEffect(() => {
    const element = scrollRef.current;
    if (element) element.scrollTop = element.scrollHeight;
  }, [entries]);

  const submit = (event: FormEvent<HTMLFormElement>) => {
    event.preventDefault();
    const text = draft.trim();
    if (!text) return;
    onSend(text);
    setDraft('');
  };

  return (
    <div className={compact ? 'chat-panel chat-panel--compact' : 'chat-panel'}>
      <div className="chat-panel__scroll" ref={scrollRef} aria-label="Room chat" role="log">
        {entries.length === 0 ? (
          <p className="chat-panel__empty">
            Chat is peer-to-peer and disappears when the room ends.
          </p>
        ) : (
          entries.map((entry) => <ChatLine key={entry.id} entry={entry} />)
        )}
      </div>
      <form className="chat-panel__composer" onSubmit={submit}>
        <input
          type="text"
          value={draft}
          maxLength={CHAT_MAX_TEXT_CHARACTERS}
          placeholder={disabled ? 'Chat connects with the room' : 'Message the room'}
          aria-label="Message the room"
          disabled={disabled}
          onChange={(event) => setDraft(event.target.value)}
        />
      </form>
    </div>
  );
}

function ChatLine({ entry }: { entry: ChatEntry }) {
  if (entry.kind === 'system') {
    return <div className="chat-panel__system">{entry.text}</div>;
  }
  return (
    <div className="chat-panel__entry">
      <span className="chat-panel__meta">
        <strong className={entry.self ? 'chat-panel__sender chat-panel__sender--self' : 'chat-panel__sender'}>
          {entry.sender}
        </strong>
        <span className="chat-panel__time">{formatTime(entry.at)}</span>
      </span>
      <span className="chat-panel__text">{entry.text}</span>
    </div>
  );
}

function formatTime(at: number): string {
  const date = new Date(at);
  return `${String(date.getHours()).padStart(2, '0')}:${String(date.getMinutes()).padStart(2, '0')}`;
}
