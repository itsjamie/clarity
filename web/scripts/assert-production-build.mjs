import { existsSync, readFileSync, readdirSync } from 'node:fs';
import { resolve } from 'node:path';

if (process.env.VITE_ENABLE_SYNTHETIC_CAPTURE === 'true') {
  throw new Error('Synthetic capture must never be enabled for a production build.');
}

for (const name of readdirSync(resolve('.')).filter((entry) => entry.startsWith('.env.production'))) {
  const path = resolve(name);
  if (existsSync(path) && /VITE_ENABLE_SYNTHETIC_CAPTURE\s*=\s*true/u.test(readFileSync(path, 'utf8'))) {
    throw new Error(`${name} enables test-only synthetic capture.`);
  }
}

console.log('production build guard: synthetic capture disabled');
