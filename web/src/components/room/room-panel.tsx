import type { ReactNode } from 'react';

export interface RoomPanelTabDefinition {
  id: string;
  label: string;
}

interface RoomPanelProps {
  tabs: readonly RoomPanelTabDefinition[];
  active: string;
  onSelect: (id: string) => void;
  meta?: string;
  children: ReactNode;
}

/** The room's right panel: a tab bar (Chat/Diagnostics/…) over one body. */
export function RoomPanel({ tabs, active, onSelect, meta, children }: RoomPanelProps) {
  return (
    <aside className="room-panel" aria-label="Room panel">
      <div className="room-panel__tabs" role="tablist">
        {tabs.map((tab) => (
          <button
            key={tab.id}
            type="button"
            role="tab"
            aria-selected={active === tab.id}
            className={active === tab.id ? 'room-panel__tab room-panel__tab--active' : 'room-panel__tab'}
            onClick={() => onSelect(tab.id)}
          >
            {tab.label}
          </button>
        ))}
        {meta ? <span className="room-panel__meta">{meta}</span> : null}
      </div>
      <div className="room-panel__body">{children}</div>
    </aside>
  );
}
