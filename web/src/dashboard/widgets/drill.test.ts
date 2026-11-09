// SPDX-License-Identifier: AGPL-3.0-only
// What each dashboard link spells (ADR-250). Whether the destination READS those keys is the route
// ledger's check (`lib/filterSpecRegistry.test.ts`, over `DRILL_SAMPLES`); this file pins the values.

import { describe, expect, it } from 'vitest';
import { decodeRange } from '../../lib/columnFilter';
import { localInputToUnix } from '../../lib/format';
import {
  alertHistoryHourDrill,
  alertsDrill,
  asEventAction,
  asEventKind,
  eventsDrill,
  folderDrill,
  hourStartOf,
  interfaceDrill,
  metricChartHref,
  nodeDrill,
  nodesInStates,
  DOWN_STATES,
} from './drill';

const params = (href: string) => new URL(href, 'http://x').searchParams;

describe('dashboard drill links', () => {
  it('open a node, its tab, and one port', () => {
    expect(nodeDrill('n1')).toBe('/nodes/n1');
    expect(nodeDrill('n1', 'events')).toBe('/nodes/n1?tab=events');
    expect(params(nodeDrill('n1', 'collection', { metric: 'cpu' })).get('collection.metric')).toBe('cpu');
    expect(interfaceDrill('n1', 7)).toBe('/nodes/n1?tab=interfaces&interfaces.port=7');
  });

  it('name node states in the filter row order, whatever order they are given in', () => {
    expect(params(nodesInStates(DOWN_STATES)).get('state')).toBe('critical,unreachable');
    expect(params(nodesInStates(['unreachable', 'critical'])).get('state')).toBe('critical,unreachable');
  });

  it('carry only the alert filters asked for', () => {
    expect(alertsDrill()).toBe('/alerts');
    expect(params(alertsDrill({ severity: 'warning' })).get('severity')).toBe('warning');
    expect(params(alertsDrill({ nodeId: 'n1' })).get('node_id')).toBe('n1');
    expect(params(alertsDrill({ flapping: true })).get('notes')).toBe('flapping');
  });

  it('open one hour as a custom range the range editor reads back', () => {
    const from = hourStartOf(1_786_001_234);
    expect(from % 3600).toBe(0);
    for (const href of [eventsDrill({ hourFromUnix: from }), alertHistoryHourDrill(from)]) {
      const p = params(href);
      const raw = p.get('at') ?? p.get('range') ?? '';
      const v = decodeRange(raw, { presets: [], defaultPreset: 'all', custom: true });
      expect(v.preset).toBe('custom');
      expect(localInputToUnix(v.from)).toBe(from);
      expect(localInputToUnix(v.to)).toBe(from + 3600);
    }
    expect(params(alertHistoryHourDrill(from)).get('phase')).toBe('fired');
  });

  it('turn only known stats keys into event filters', () => {
    expect(asEventKind('trap')).toBe('trap');
    expect(asEventKind('netflow')).toBeUndefined();
    expect(asEventAction('fired')).toBe('fired');
    expect(asEventAction('')).toBeUndefined();
    expect(eventsDrill({ kind: undefined })).toBe('/events');
  });

  it('open a folder in the tree', () => {
    expect(params(folderDrill('g1')).get('sel')).toBe('group:g1');
  });

  it('give the metric chart a link only once a node is chosen', () => {
    expect(metricChartHref(undefined)).toBeUndefined();
    expect(metricChartHref({ nodeId: 'n1' })).toBe('/nodes/n1');
    expect(metricChartHref({ nodeId: 'n1', metric: 'cpu' })).toBe(
      '/nodes/n1?tab=collection&collection.metric=cpu',
    );
  });
});
