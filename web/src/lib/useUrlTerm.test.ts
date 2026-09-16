// SPDX-License-Identifier: AGPL-3.0-only
// @vitest-environment jsdom
//
// A search term held in the URL behind a locally-drafted box (ADR-153). The codec underneath is
// `filterParams.ts` and is tested there; what this file covers is the adopt/commit rule, because
// every way of getting it wrong shows only on a reload, a Back press, or mid-keystroke:
//
//  - adopting the hook's own (trimmed) write eats the trailing space the operator just typed;
//  - adopting on "URL differs from the box" rewinds the box whenever a write was lost;
//  - committing from the caller's clear-all snapshot restores the filters clear-all just removed.

import { createElement, type ReactNode } from 'react';
import { act, renderHook } from '@testing-library/react';
import { MemoryRouter, useLocation, useNavigate, useSearchParams } from 'react-router-dom';
import { describe, expect, it } from 'vitest';
import { useUrlTerm } from './useUrlTerm';

const useProbe = () => {
  const location = useLocation();
  const [params, setParams] = useSearchParams();
  return {
    ...useUrlTerm('q'),
    navigate: useNavigate(),
    params,
    setParams,
    search: location.search,
    path: location.pathname,
    /** A new key per navigation, `replace` included — the way to see that a write happened. */
    locKey: location.key,
  };
};

function mount(entries: string[]) {
  const wrapper = ({ children }: { children: ReactNode }) =>
    createElement(MemoryRouter, { initialEntries: entries, initialIndex: entries.length - 1 }, children);
  return renderHook(useProbe, { wrapper });
}

describe('useUrlTerm', () => {
  it('starts from the term the URL arrived with', () => {
    const { result } = mount(['/nodes?q=TDC&state=critical']);
    expect(result.current.draft).toBe('TDC');
  });

  it('starts empty on a bare URL', () => {
    const { result } = mount(['/nodes']);
    expect(result.current.draft).toBe('');
  });

  it('keeps typing out of the URL until the caller commits', () => {
    const { result } = mount(['/nodes?state=critical']);
    const before = result.current.locKey;
    act(() => result.current.setDraft('TD'));
    act(() => result.current.setDraft('TDC'));
    expect(result.current.draft).toBe('TDC');
    expect(result.current.locKey, 'a keystroke wrote the URL').toBe(before);
    expect(result.current.search).toBe('?state=critical');
  });

  it('commits the trimmed term, beside the keys already there', () => {
    const { result } = mount(['/nodes?state=critical']);
    act(() => result.current.setDraft(' TDC '));
    act(() => result.current.commit(' TDC '));
    const params = new URLSearchParams(result.current.search);
    expect(params.get('q')).toBe('TDC');
    expect(params.get('state')).toBe('critical');
  });

  it('does not rewrite the box with its own trimmed write', () => {
    // The operator is halfway through "sw 01": the settle lands on "sw " and the URL gets "sw".
    // Adopting that would delete the space under the caret.
    const { result } = mount(['/nodes']);
    act(() => result.current.setDraft('sw '));
    act(() => result.current.commit('sw '));
    expect(result.current.search).toBe('?q=sw');
    expect(result.current.draft).toBe('sw ');
  });

  it('deletes the key for a blank term rather than writing an empty one', () => {
    const { result } = mount(['/nodes?q=TDC&state=critical']);
    act(() => result.current.setDraft('   '));
    act(() => result.current.commit('   '));
    expect(result.current.search).toBe('?state=critical');
  });

  it('writes nothing when the settled term is already the URL', () => {
    // The caller commits from an effect on its settled value, which re-runs for reasons of its own.
    const { result } = mount(['/nodes?q=TDC']);
    const before = result.current.locKey;
    act(() => result.current.commit('TDC'));
    expect(result.current.locKey).toBe(before);
  });

  it('replaces rather than pushes, so Back leaves the page', () => {
    const { result } = mount(['/alerts', '/nodes']);
    act(() => result.current.commit('s'));
    act(() => result.current.commit('sw'));
    expect(result.current.search).toBe('?q=sw');
    act(() => {
      result.current.navigate(-1);
    });
    expect(result.current.path).toBe('/alerts');
  });

  it('adopts a term that arrives from outside — Back, a link', () => {
    const { result } = mount(['/nodes?q=core', '/nodes?q=TDC']);
    expect(result.current.draft).toBe('TDC');
    act(() => {
      result.current.navigate(-1);
    });
    expect(result.current.draft).toBe('core');
    act(() => {
      result.current.navigate('/nodes');
    });
    expect(result.current.draft).toBe('');
  });

  it('does not rewind the box when its write is lost to a later one from an older snapshot', () => {
    // Two `setSearchParams` in one tick: the second was built before the term was committed, so it
    // lands last and the URL never shows "TDC". The URL's term did not change — so nothing was
    // navigated *to*, and the box must keep what the operator typed.
    const { result } = mount(['/nodes']);
    const stale = new URLSearchParams(result.current.params);
    stale.set('sel', 'node:n-1');
    act(() => {
      result.current.setDraft('TDC');
      result.current.commit('TDC');
      result.current.setParams(stale, { replace: true });
    });
    expect(result.current.search).toBe('?sel=node%3An-1');
    expect(result.current.draft).toBe('TDC');
  });

  it('lets a handler clear the term inside its own single write', () => {
    // "Clear all filters": the term and every other key go in ONE write. The commit that follows
    // (the caller's settled value going blank) must then write nothing — a write from the pre-clear
    // snapshot would put `state=critical` straight back.
    //
    // The commit is issued in the same tick as the clear, before a render has seen the new URL —
    // the order a settle effect can run in when the box's update and the router's are not batched
    // together. Its snapshot is therefore still `?q=TDC&state=critical`.
    const { result } = mount(['/nodes?q=TDC&state=critical']);
    act(() => {
      const params = new URLSearchParams(result.current.params);
      params.delete('state');
      result.current.assign(params, '');
      result.current.setParams(params, { replace: true });
      result.current.commit('');
    });
    expect(result.current.search, 'the settle restored what clear-all removed').toBe('');
    expect(result.current.draft).toBe('');
  });
});
