// SPDX-License-Identifier: AGPL-3.0-only
import { describe, expect, it } from 'vitest';
import { EVENT_LISTENER_KINDS, eventListeners } from './listeners';

describe('eventListeners', () => {
  it('says nothing when no poller has bound anything', () => {
    expect(eventListeners([])).toEqual([]);
    expect(eventListeners([{ id: 'poller-tokyo', listeners: [] }])).toEqual([]);
  });

  it('splits a label into its kind and the address behind it', () => {
    expect(eventListeners([{ id: 'poller-tokyo', listeners: ['syslog:0.0.0.0:1514'] }])).toEqual([
      { kind: 'syslog', bind: '0.0.0.0:1514', pollers: ['poller-tokyo'] },
    ]);
  });

  it('keeps an IPv6 bind whole — the split is on the FIRST colon', () => {
    // `syslog:[::]:1514` read greedily gives a bind of `[`, which is the bug this asserts against.
    // Both address families are in scope everywhere in this codebase, so this is a real input.
    expect(eventListeners([{ id: 'p1', listeners: ['syslog:[::]:1514'] }])).toEqual([
      { kind: 'syslog', bind: '[::]:1514', pollers: ['p1'] },
    ]);
  });

  it('names every poller listening on the same endpoint, once each', () => {
    const bindings = eventListeners([
      { id: 'poller-tokyo', listeners: ['syslog:0.0.0.0:1514'] },
      { id: 'poller-osaka', listeners: ['syslog:0.0.0.0:1514'] },
      // A duplicate label from one poller must not name it twice in the sentence.
      { id: 'poller-tokyo', listeners: ['syslog:0.0.0.0:1514'] },
    ]);
    expect(bindings).toEqual([
      { kind: 'syslog', bind: '0.0.0.0:1514', pollers: ['poller-tokyo', 'poller-osaka'] },
    ]);
  });

  it('orders by kind first, then by the bind it saw first', () => {
    const bindings = eventListeners([
      { id: 'p1', listeners: ['trap:0.0.0.0:1162', 'syslog:0.0.0.0:1514'] },
      { id: 'p2', listeners: ['syslog:0.0.0.0:5514'] },
    ]);
    expect(bindings.map((b) => `${b.kind} ${b.bind}`)).toEqual([
      'syslog 0.0.0.0:1514',
      'syslog 0.0.0.0:5514',
      'trap 0.0.0.0:1162',
    ]);
  });

  it('drops a kind whose traffic does not land in this log', () => {
    // Flow goes to ClickHouse and is read on the node's Flow tab. Listing its endpoint here would
    // invite "I export to that address and see no events, so reception is broken".
    expect(EVENT_LISTENER_KINDS).not.toContain('flow');
    const bindings = eventListeners([
      { id: 'p1', listeners: ['flow:0.0.0.0:2055', 'syslog:0.0.0.0:1514'] },
    ]);
    expect(bindings).toEqual([{ kind: 'syslog', bind: '0.0.0.0:1514', pollers: ['p1'] }]);
  });

  it('drops a label it cannot read instead of rendering half of one', () => {
    // A newer poller advertising a shape this build has never seen degrades to silence. `syslog:`
    // with nothing after it is the one that would otherwise print a kind with an empty address.
    expect(
      eventListeners([{ id: 'p1', listeners: ['syslog', 'syslog:', ':1514', ''] }]),
    ).toEqual([]);
  });
});
