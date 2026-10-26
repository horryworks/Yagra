// SPDX-License-Identifier: AGPL-3.0-only
import { describe, expect, it } from 'vitest';
import { lazyBackend, unwrapModule } from './i18nBackend';

/** Run one `read` and resolve with what its callback was given. */
function read(
  load: Parameters<typeof lazyBackend>[0],
  language = 'ja',
  namespace = 'nodes',
): Promise<{ err: unknown; data: unknown }> {
  return new Promise((resolve) => {
    const backend = lazyBackend(load) as unknown as {
      read: (l: string, n: string, cb: (err: unknown, data: unknown) => void) => void;
    };
    backend.read(language, namespace, (err, data) => resolve({ err, data }));
  });
}

describe('lazyBackend', () => {
  it('is an i18next backend', () => {
    expect(lazyBackend(() => Promise.resolve({})).type).toBe('backend');
  });

  it('asks the loader for the language and namespace i18next names', async () => {
    const asked: string[] = [];
    await read((l, n) => {
      asked.push(`${l}/${n}`);
      return Promise.resolve({});
    }, 'ja', 'settings-auth');
    expect(asked).toEqual(['ja/settings-auth']);
  });

  it('answers with the object inside a JSON module', async () => {
    const got = await read(() => Promise.resolve({ default: { title: 'ノード' } }));
    expect(got).toEqual({ err: null, data: { title: 'ノード' } });
  });

  it('answers with a plain object as it is', async () => {
    const got = await read(() => Promise.resolve({ title: 'ノード' }));
    expect(got.data).toEqual({ title: 'ノード' });
  });

  // A chunk that fails to load must reach i18next as an error, so it falls back to English
  // rather than caching an empty namespace.
  it('passes a failed load on as an error', async () => {
    const boom = new Error('chunk failed');
    expect(await read(() => Promise.reject(boom))).toEqual({ err: boom, data: false });
    const thrown = await read(() => {
      throw boom;
    });
    expect(thrown).toEqual({ err: boom, data: false });
  });
});

describe('unwrapModule', () => {
  it('keeps an object that merely has no default key', () => {
    expect(unwrapModule({ a: 1 })).toEqual({ a: 1 });
  });
});
