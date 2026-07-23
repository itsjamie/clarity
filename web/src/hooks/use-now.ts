import { useEffect, useState } from 'react';

export function useNow(intervalMilliseconds = 1_000): number {
  const [now, setNow] = useState(Date.now());
  useEffect(() => {
    const timer = window.setInterval(() => setNow(Date.now()), intervalMilliseconds);
    return () => window.clearInterval(timer);
  }, [intervalMilliseconds]);
  return now;
}
