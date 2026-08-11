import type { ReactNode } from 'react';

interface RoomHeaderProps {
  live: boolean;
  paused?: boolean;
  title: string;
  meta: string;
  children?: ReactNode;
}

export function RoomHeader({ live, paused = false, title, meta, children }: RoomHeaderProps) {
  const dotClass = live
    ? 'room-header__dot room-header__dot--live'
    : paused
      ? 'room-header__dot room-header__dot--paused'
      : 'room-header__dot';
  return (
    <header className="room-header">
      <span className="room-header__title">
        <i className={dotClass} aria-hidden="true" />
        {title}
      </span>
      <span className="room-header__meta">{meta}</span>
      <span className="room-header__spacer" aria-hidden="true" />
      <div className="room-header__actions">{children}</div>
    </header>
  );
}
