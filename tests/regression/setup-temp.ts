import { afterAll } from 'vitest';
import { flushFileTemps } from './file-temp.ts';

// A root-level hook: it runs once after all tests of each test file (Vitest's forks workers do not run `process.on('exit')`).
afterAll(flushFileTemps);
