// SPDX-License-Identifier: AGPL-3.0-only
// The i18next backend that loads a namespace on demand (ADR-214 decision 2; it was the
// `i18next-resources-to-backend` package). i18next asks it only for a language/namespace that is
// not already in `resources`, so at runtime that means the non-English locales.
//
// The contract is i18next's: `type: 'backend'`, and `read(language, namespace, callback)` answering
// the namespace's object or an error. A JSON file loaded with `import()` arrives as a module whose
// `default` is the object, so that is unwrapped here, as the package did.

import type { BackendModule, ReadCallback } from 'i18next';

/** Loads one namespace of one language. */
export type NamespaceLoader = (language: string, namespace: string) => Promise<unknown>;

/** A backend that answers each read with what `load` resolves to. */
export function lazyBackend(load: NamespaceLoader): BackendModule {
  return {
    type: 'backend',
    init() {},
    read(language: string, namespace: string, callback: ReadCallback) {
      let pending: Promise<unknown>;
      try {
        pending = load(language, namespace);
      } catch (err) {
        callback(err as Error, false);
        return;
      }
      pending.then(
        (loaded) => callback(null, unwrapModule(loaded)),
        (err: unknown) => callback(err as Error, false),
      );
    },
  };
}

/** The object inside a JSON module (`{ default: {...} }`), or the value itself. */
export function unwrapModule(loaded: unknown): Record<string, unknown> {
  if (loaded !== null && typeof loaded === 'object' && 'default' in loaded) {
    return (loaded as { default: Record<string, unknown> }).default;
  }
  return loaded as Record<string, unknown>;
}
