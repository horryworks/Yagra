// SPDX-License-Identifier: AGPL-3.0-only
// @vitest-environment jsdom
//
// A tab or a chip held in the URL (ADR-153). The codec's rules are `filterParams.test.ts`'s; this
// covers only what the binding adds — that it reads the router, writes beside the keys already
// there, and does not push history.

import { createElement, type ReactNode } from 'react';
import { act, renderHook } from '@testing-library/react';
import { MemoryRouter, useLocation, useNavigate } from 'react-router-dom';
import { describe, expect, it } from 'vitest';
import { useEnumParam } from './useEnumParam';

const TABS = ['saved', 'templates', 'schedules'] as const;

const useProbe = () => {
  const [tab, setTab] = useEnumParam('tab', TABS, 'saved');
  return { tab, setTab, navigate: useNavigate(), search: useLocation().search, path: useLocation().pathname };
};

function mount(entries: string[]) {
  const wrapper = ({ children }: { children: ReactNode }) =>
    createElement(MemoryRouter, { initialEntries: entries, initialIndex: entries.length - 1 }, children);
  return renderHook(useProbe, { wrapper });
}

describe('useEnumParam', () => {
  it('reads the value the URL names', () => {
    expect(mount(['/dashboard/reports?tab=schedules']).result.current.tab).toBe('schedules');
  });

  it('reads the fallback for a bare URL and for a value this build does not offer', () => {
    expect(mount(['/dashboard/reports']).result.current.tab).toBe('saved');
    expect(mount(['/dashboard/reports?tab=archive']).result.current.tab).toBe('saved');
  });

  it('writes beside the keys already there, and deletes itself at the fallback', () => {
    const { result } = mount(['/dashboard/reports?saved.name=weekly']);
    act(() => result.current.setTab('templates'));
    const params = new URLSearchParams(result.current.search);
    expect(params.get('tab')).toBe('templates');
    expect(params.get('saved.name')).toBe('weekly');

    act(() => result.current.setTab('saved'));
    expect(result.current.search).toBe('?saved.name=weekly');
  });

  it('replaces rather than pushes, so Back leaves the page', () => {
    const { result } = mount(['/nodes', '/dashboard/reports']);
    act(() => result.current.setTab('templates'));
    act(() => result.current.setTab('schedules'));
    act(() => {
      result.current.navigate(-1);
    });
    expect(result.current.path).toBe('/nodes');
  });
});
