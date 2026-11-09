// SPDX-License-Identifier: AGPL-3.0-only
// Whether this part of the page may link to other screens (ADR-250 decision 4).
//
// The anonymous public board draws the same widgets as a signed-in board, but a stranger cannot
// open the screens they link to: the link changes the URL and the visitor stays where they were.
// So the board says "no links here" once, around itself, and every link that asks draws its text
// instead. The default is true, so a screen that never sets it is unaffected.

import { createContext, useContext } from 'react';

export const LinksEnabledContext = createContext(true);

/** False inside a surface that must not link out (the public board, and its anonymous preview). */
export function useLinksEnabled(): boolean {
  return useContext(LinksEnabledContext);
}
