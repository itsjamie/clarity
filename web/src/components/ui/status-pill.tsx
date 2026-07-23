import type { ReactNode } from 'react';

interface StatusPillProps {
  tone?: 'neutral' | 'success' | 'warning' | 'danger' | 'accent';
  children: ReactNode;
}

export function StatusPill({ tone = 'neutral', children }: StatusPillProps) {
  return <span className={`status-pill status-pill--${tone}`}>{children}</span>;
}
