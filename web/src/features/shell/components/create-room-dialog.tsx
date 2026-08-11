import { useEffect } from 'react';

import { CreateRoomForm } from '@/features/room-creation/components/create-room-form';

export function CreateRoomDialog({ onClose }: { onClose: () => void }) {
  useEffect(() => {
    const onKey = (event: KeyboardEvent) => {
      if (event.key === 'Escape') onClose();
    };
    window.addEventListener('keydown', onKey);
    return () => window.removeEventListener('keydown', onKey);
  }, [onClose]);

  return (
    <div className="shell-overlay" onClick={onClose}>
      <div
        className="shell-dialog"
        role="dialog"
        aria-modal="true"
        aria-label="Create a room"
        onClick={(event) => event.stopPropagation()}
      >
        <div className="shell-dialog__header">
          <span className="shell-dialog__eyebrow">Create a room</span>
          <button type="button" className="shell-dialog__close" aria-label="Close" onClick={onClose}>
            ×
          </button>
        </div>
        <CreateRoomForm />
      </div>
    </div>
  );
}
