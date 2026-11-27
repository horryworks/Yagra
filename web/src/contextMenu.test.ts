// SPDX-License-Identifier: AGPL-3.0-only
// A right-click menu is given through `useMenuGesture`, never by hand (ADR-271 decision 5).
//
// A menu wired with a bare `onContextMenu=` — or a `contextmenu` listener — opens for a mouse and
// for Android, and never for an iPhone: iOS Safari fires no `contextmenu` on a long press. That is
// how the inventory tree's whole menu was out of reach on a phone. `components/ui/menuGesture.ts`
// gives the right button and the long press from one call, so a menu added through it reaches a
// finger without anyone having to remember that it must.

import { describe, expect, it } from 'vitest';
import { codeOnly, readSources } from './testSupport/sources';

/** The one file that may handle the event itself. */
const GESTURE = 'components/ui/menuGesture.ts';

/** A handler written in JSX, or a listener added by hand. */
const HAND_WIRED = [/\bonContextMenu(?:Capture)?\s*=/, /addEventListener\(\s*['"`]contextmenu['"`]/];

const handWired = (src: string): boolean => HAND_WIRED.some((re) => re.test(codeOnly(src)));

describe('right-click menus', () => {
  const files = readSources();

  it('recognises a hand-wired menu, and not the gesture spread', () => {
    expect(handWired('<div onContextMenu={(e) => open(e)} />')).toBe(true);
    expect(handWired('<div onContextMenuCapture={x} />')).toBe(true);
    expect(handWired("el.addEventListener('contextmenu', h);")).toBe(true);
    expect(handWired('<div {...gesture.bind(open)} />')).toBe(false);
    expect(handWired('  onContextMenu: (e) => {')).toBe(false);
    expect(handWired('// onContextMenu={x} in a comment')).toBe(false);
  });

  it('are wired through useMenuGesture, never by hand', () => {
    const offenders = files.filter(([rel, src]) => rel !== GESTURE && handWired(src)).map(([rel]) => rel);
    expect(offenders).toEqual([]);
  });

  it('read the source tree, and found the menus that exist', () => {
    // A walk that stopped finding files would pass the check above over nothing.
    expect(files.length).toBeGreaterThan(400);
    const users = files.filter(([, src]) => /\buseMenuGesture\(\)/.test(codeOnly(src)));
    expect(users.map(([rel]) => rel)).toContain('components/NodeTree/NodeTree.tsx');
  });
});
