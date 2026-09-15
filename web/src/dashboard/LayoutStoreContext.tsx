// SPDX-License-Identifier: AGPL-3.0-only
// Store injection for the dashboard widget machinery. `WidgetFrame` and `CatalogModal` are reused
// by both the per-user My Dashboard and the global Shared Dashboard; each page provides its active
// store via this context so those components edit the right board. The context itself and the hook
// that reads it live in `layoutStoreHook.ts`, so this file exports only the provider.

import type { ReactNode } from 'react';
import type { LayoutStoreApi } from './layoutStore';
import { LayoutStoreContext } from './layoutStoreHook';

export function LayoutStoreProvider({
  store,
  children,
}: {
  store: LayoutStoreApi;
  children: ReactNode;
}) {
  return <LayoutStoreContext.Provider value={store}>{children}</LayoutStoreContext.Provider>;
}
