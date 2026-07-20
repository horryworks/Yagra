// SPDX-License-Identifier: AGPL-3.0-only
// Store injection for the dashboard widget machinery. `WidgetFrame` and `CatalogModal` are reused
// by both the per-user My Dashboard and the global Shared Dashboard; each page provides its active
// store via this context so those components edit the right board. Defaults to the My Dashboard
// store so a component rendered without a provider still works.

import { createContext, useContext, type ReactNode } from 'react';
import { useLayoutStore, type LayoutStoreApi } from './layoutStore';

const LayoutStoreContext = createContext<LayoutStoreApi>(useLayoutStore);

export function LayoutStoreProvider({
  store,
  children,
}: {
  store: LayoutStoreApi;
  children: ReactNode;
}) {
  return <LayoutStoreContext.Provider value={store}>{children}</LayoutStoreContext.Provider>;
}

/** The active layout store hook for the current board subtree. */
export function useLayoutStoreContext(): LayoutStoreApi {
  return useContext(LayoutStoreContext);
}
