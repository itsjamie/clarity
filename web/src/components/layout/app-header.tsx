import { Link } from 'react-router-dom';

import clarityLogoUrl from '@/assets/clarity-logo.svg';

interface AppHeaderProps {
  assurance?: string;
}

export function AppHeader({ assurance = 'Media stays peer to peer' }: AppHeaderProps) {
  return (
    <header className="app-header">
      <Link className="wordmark" to="/" aria-label="Clarity Share home">
        <span className="wordmark__mark" aria-hidden="true">
          <img src={clarityLogoUrl} alt="" />
        </span>
        <span>Clarity Share</span>
      </Link>
      <div className="app-header__assurance">
        <span className="assurance-dot" aria-hidden="true" />
        {assurance}
      </div>
    </header>
  );
}
