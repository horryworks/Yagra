import { describe, expect, it } from 'vitest';
import { isChunkLoadError } from './chunkLoad';

describe('isChunkLoadError', () => {
  // The messages each engine actually rejects a failed dynamic import with.
  it.each([
    ['Chromium', 'Failed to fetch dynamically imported module: https://yagra.example/assets/settings-AbC123.js'],
    ['Firefox', 'error loading dynamically imported module: https://yagra.example/assets/settings-AbC123.js'],
    ['Safari', 'Importing a module script failed.'],
    ['Vite CSS preload', 'Unable to preload CSS for /assets/settings-AbC123.css'],
    [
      'HTML served for a module',
      'Failed to load module script: Expected a JavaScript-or-Wasm module script but the server responded with a MIME type of "text/html".',
    ],
  ])('recognises the %s wording', (_engine, message) => {
    expect(isChunkLoadError(new TypeError(message))).toBe(true);
  });

  it('recognises an error named ChunkLoadError whatever its message', () => {
    const e = new Error('Loading chunk 7 failed.');
    e.name = 'ChunkLoadError';
    expect(isChunkLoadError(e)).toBe(true);
  });

  // Anything else is a bug in a screen, and must keep reaching whatever handled it before.
  it.each([
    new TypeError("Cannot read properties of undefined (reading 'map')"),
    new Error('Failed to fetch'),
    new RangeError('Invalid time value'),
  ])('leaves an ordinary render error alone: %s', (e) => {
    expect(isChunkLoadError(e)).toBe(false);
  });

  it('is false for things that are not errors', () => {
    expect(isChunkLoadError('Failed to fetch dynamically imported module')).toBe(false);
    expect(isChunkLoadError(undefined)).toBe(false);
    expect(isChunkLoadError(null)).toBe(false);
  });
});
