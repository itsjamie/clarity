import { Link } from 'react-router-dom';

export function FatalError({ title, message }: { title: string; message: string }) {
  return (
    <main className="centered-state">
      <p className="eyebrow">Clarity Share</p>
      <h1>{title}</h1>
      <p>{message}</p>
      <Link className="button button--primary" to="/">
        Return home
      </Link>
    </main>
  );
}
