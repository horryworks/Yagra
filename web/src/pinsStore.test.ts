// SPDX-License-Identifier: AGPL-3.0-only
// The pins store's judgement (ADR-146, `pinsStore.ts`): when the controls exist at all, that a toggle
// shows at once and is put back when refused, and that sign-out cannot be overwritten by a late answer.

import { beforeEach, describe, expect, it, vi } from 'vitest';

const getPins = vi.fn();
const pinNode = vi.fn();
const unpinNode = vi.fn();
const pinGroup = vi.fn();
const unpinGroup = vi.fn();
const getToken = vi.fn();

vi.mock('./services/api', () => ({
  api: {
    getPins: () => getPins(),
    pinNode: (id: string) => pinNode(id),
    unpinNode: (id: string) => unpinNode(id),
    pinGroup: (id: string) => pinGroup(id),
    unpinGroup: (id: string) => unpinGroup(id),
  },
  getToken: () => getToken(),
}));

import { usePinsStore } from './pinsStore';

const row = (id: string, group_id: string | null) => ({ id, group_id }) as never;

beforeEach(() => {
  for (const f of [getPins, pinNode, unpinNode, pinGroup, unpinGroup]) f.mockReset();
  getToken.mockReset().mockReturnValue('session-token');
  usePinsStore.getState().reset();
});

describe('load', () => {
  it('does not call the server when nobody is signed in', async () => {
    getToken.mockReturnValue(null);
    await usePinsStore.getState().load();
    expect(getPins).not.toHaveBeenCalled();
    expect(usePinsStore.getState().status).toBe('idle');
  });

  it('holds the folders and nodes the server returned', async () => {
    getPins.mockResolvedValue({ group_ids: ['g1'], nodes: [row('n1', 'g2')] });
    await usePinsStore.getState().load();
    const s = usePinsStore.getState();
    expect(s.status).toBe('ready');
    expect([...s.groupIds]).toEqual(['g1']);
    expect([...s.nodeIds]).toEqual(['n1']);
    expect(s.nodes).toHaveLength(1);
  });

  it('marks pins unavailable when the first load fails, so no control is drawn', async () => {
    // The N-1 core answers 404: a pin control there would press into nothing.
    getPins.mockRejectedValue(new Error('404'));
    await usePinsStore.getState().load();
    expect(usePinsStore.getState().status).toBe('unavailable');
  });

  it('keeps what is on screen when a later re-read fails', async () => {
    getPins.mockResolvedValueOnce({ group_ids: ['g1'], nodes: [] });
    await usePinsStore.getState().load();
    getPins.mockRejectedValue(new Error('network'));
    await usePinsStore.getState().load();
    expect(usePinsStore.getState().status).toBe('ready');
    expect([...usePinsStore.getState().groupIds]).toEqual(['g1']);
  });

  it('drops an answer that arrives after sign-out', async () => {
    let answer: (v: unknown) => void = () => undefined;
    getPins.mockReturnValue(new Promise((r) => (answer = r)));
    const pending = usePinsStore.getState().load();
    usePinsStore.getState().reset();
    answer({ group_ids: ['previous-account'], nodes: [] });
    await pending;
    expect(usePinsStore.getState().status).toBe('idle');
    expect(usePinsStore.getState().groupIds.size).toBe(0);
  });
});

describe('toggling', () => {
  it('shows a folder pin before the server answers', async () => {
    let done: () => void = () => undefined;
    pinGroup.mockReturnValue(new Promise<void>((r) => (done = r)));
    const pending = usePinsStore.getState().setGroupPinned('g1', true);
    expect(usePinsStore.getState().groupIds.has('g1')).toBe(true);
    done();
    await pending;
    expect(pinGroup).toHaveBeenCalledWith('g1');
  });

  it('puts the mark back and rejects when the server refuses', async () => {
    getPins.mockResolvedValue({ group_ids: [], nodes: [row('n1', null)] });
    await usePinsStore.getState().load();
    unpinNode.mockRejectedValue(new Error('500'));
    await expect(usePinsStore.getState().setNodePinned('n1', false)).rejects.toThrow('500');
    expect(usePinsStore.getState().nodeIds.has('n1')).toBe(true);
  });

  it('re-reads after pinning a node, so its row arrives for the tree', async () => {
    getPins.mockResolvedValue({ group_ids: [], nodes: [] });
    await usePinsStore.getState().load();
    pinNode.mockResolvedValue(undefined);
    getPins.mockResolvedValue({ group_ids: [], nodes: [row('n1', 'g1')] });
    await usePinsStore.getState().setNodePinned('n1', true);
    expect(usePinsStore.getState().nodes).toHaveLength(1);
    expect(getPins).toHaveBeenCalledTimes(2);
  });
});
