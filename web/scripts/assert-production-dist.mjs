import { readFileSync, readdirSync, statSync } from 'node:fs';
import { join, resolve } from 'node:path';

const marker = 'CLARITY_SYNTHETIC_CAPTURE_TEST_ONLY';
const root = resolve('dist');

function files(directory) {
  return readdirSync(directory).flatMap((name) => {
    const path = join(directory, name);
    return statSync(path).isDirectory() ? files(path) : [path];
  });
}

for (const path of files(root)) {
  if (readFileSync(path).includes(marker)) {
    throw new Error(`Production output contains synthetic capture code: ${path}`);
  }
}

console.log('production dist guard: synthetic capture code absent');
