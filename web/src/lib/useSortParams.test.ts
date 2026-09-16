// SPDX-License-Identifier: AGPL-3.0-only
// @vitest-environment jsdom
//
// A table's sort in the URL (ADR-153). The rules are `tableSort.test.ts`'s; this covers what the
// binding adds — it reads the router, writes beside the filter keys, and does not push history.

import { createElement, type ReactNode } from 'react';
import { act, renderHook } from '@testing-library/react';
import { MemoryRouter, useLocation, useNavigate } from 'react-router-dom';
import { describe, expect, it } from 'vitest';
import type { SortState } from './tableSort';
import { useSortParams } from './useSortParams';

const FALLBACK: SortState = { by: 'created', dir: 'desc' };
const SORTABLE = ['name', 'created'];

const useProbe = () => {
  const [sort, setSort] = useSortParams(SORTABLE, FALLBACK);
  const location = useLocation();
  return { sort, setSort, navigate: useNavigate(), search: location.search, path: location.pathname };
};

function mount(entries: string[]) {
  const wrapper = ({ children }: { children: ReactNode }) =>
    createElement(MemoryRouter, { initialEntries: entries, initialIndex: entries.length - 1 }, children);
  return renderHook(useProbe, { wrapper });
}

describe('useSortParams', () => {
  it('reads the sort the URL arrived with', () => {
    expect(mount(['/settings/api-tokens?sort=name&dir=asc']).result.current.sort).toEqual({
      by: 'name',
      dir: 'asc',
    });
  });

  it('writes beside the filter keys, and removes itself at the default', () => {
    const { result } = mount(['/settings/api-tokens?state=active']);
    act(() => result.current.setSort({ by: 'name', dir: 'desc' }));
    const params = new URLSearchParams(result.current.search);
    expect(params.get('sort')).toBe('name');
    expect(params.get('dir')).toBe('desc');
    expect(params.get('state')).toBe('active');

    act(() => result.current.setSort(FALLBACK));
    expect(result.current.search).toBe('?state=active');
  });

  it('replaces rather than pushes, so Back leaves the page', () => {
    const { result } = mount(['/nodes', '/settings/api-tokens']);
    act(() => result.current.setSort({ by: 'name', dir: 'asc' }));
    act(() => result.current.setSort({ by: 'name', dir: 'desc' }));
    act(() => {
      result.current.navigate(-1);
    });
    expect(result.current.path).toBe('/nodes');
  });
});
