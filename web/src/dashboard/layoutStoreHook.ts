// SPDX-License-Identifier: AGPL-3.0-only
// The context `LayoutStoreProvider` fills, and the hook the widget machinery reads it through. Kept
// apart from the provider so `LayoutStoreContext.tsx` exports only a component (react-refresh's
// rule). Defaults to the My Dashboard store so a component rendered without a provider still works.

import { createContext, useContext } from 'react';
import { useLayoutStore, type LayoutStoreApi } from './layoutStore';

export const LayoutStoreContext = createContext<LayoutStoreApi>(useLayoutStore);

/** The active layout store hook for the current board subtree. */
export function useLayoutStoreContext(): LayoutStoreApi {
  return useContext(LayoutStoreContext);
}
