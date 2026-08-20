// SPDX-License-Identifier: AGPL-3.0-only
// The picker's failure mode is not an error — it is a list that looks complete and is not, or one
// that offers a choice the save will refuse. Both are decided here.

import { describe, expect, it } from 'vitest';
import type { MibCatalogEntry } from '../types/api';
import {
  filterMetricOptions,
  groupMetricOptions,
  metricOptions,
  offersCustomName,
} from './metricCatalog';
import { LIVENESS_METRIC } from './format';

const LABELS = {
  checks: "Yagra's own checks",
  derived: 'Computed by Yagra',
  standard: 'Standard SNMP',
  liveness: 'Reachability',
  // Only the two the tests name — everything else reads as "no sentence", which is the fallback
  // path and needs exercising too.
  meaning: (key: string) =>
    ({
      'metrics:meaning.if_oper_status': 'Whether the port is up. 1 = up, 2 = down.',
      'metrics:meaning.cisco_env_temp': 'A chassis temperature sensor reading in degrees Celsius.',
    })[key] ?? '',
};

const entry = (over: Partial<MibCatalogEntry>): MibCatalogEntry => ({
  id: '00000000-0000-0000-0000-000000000000',
  metric_name: 'x',
  oid: '1.2.3',
  collection: 'table',
  metric_kind: 'gauge',
  vendor: null,
  description: null,
  ...over,
});

const CATALOG: MibCatalogEntry[] = [
  entry({ metric_name: 'if_oper_status', oid: '1.3.6.1.2.1.2.2.1.8' }),
  entry({ metric_name: 'if_hc_in_octets', oid: '1.3.6.1.2.1.31.1.1.1.6', metric_kind: 'counter' }),
  entry({
    metric_name: 'cisco_env_temp',
    oid: '1.3.6.1.4.1.9.9.13.1.3.1.3',
    vendor: 'Cisco IOS/IOS-XE health',
  }),
  entry({ metric_name: 'acme_widget_temp', oid: '1.3.6.1.4.1.99999.1', vendor: 'Acme' }),
];

describe('metricOptions', () => {
  it('drops a counter, because the API refuses a threshold on one', () => {
    // `POST /thresholds` answers `counter_metric` (400). Offering it would be offering a choice
    // the save rejects — the exact failure this picker exists to remove.
    const names = metricOptions(CATALOG, LABELS).map((o) => o.name);
    expect(names).not.toContain('if_hc_in_octets');
  });

  it('offers the gauges it was given', () => {
    // The accepting side. A "drops the counter" test alone passes on an implementation that
    // returns nothing at all.
    const names = metricOptions(CATALOG, LABELS).map((o) => o.name);
    expect(names).toContain('if_oper_status');
    expect(names).toContain('cisco_env_temp');
    expect(names).toContain('acme_widget_temp');
  });

  it("leads with Yagra's own checks, which have no catalog row", () => {
    const opts = metricOptions(CATALOG, LABELS);
    expect(opts[0].name).toBe(LIVENESS_METRIC);
    expect(opts[0].group).toBe("Yagra's own checks");
    // The sentinel is an internal token; showing it raw would teach a string nobody may type.
    expect(opts[0].label).toBe('Reachability');
    expect(opts.map((o) => o.name)).toContain('icmp_rtt_ms');
  });

  it('files a catalog row under its metric set, and a vendor-less one under Standard SNMP', () => {
    const by = (n: string) => metricOptions(CATALOG, LABELS).find((o) => o.name === n)!;
    expect(by('cisco_env_temp').group).toBe('Cisco IOS/IOS-XE health');
    expect(by('if_oper_status').group).toBe('Standard SNMP');
  });

  it('falls back to the catalog entry description when there is no translated sentence', () => {
    // The order matters: a translated sentence wins, because the stored column is single-language.
    const withDesc = [entry({ metric_name: 'acme_x', description: 'Widgets per fortnight.' })];
    expect(metricOptions(withDesc, LABELS).find((o) => o.name === 'acme_x')!.meaning).toBe(
      'Widgets per fortnight.',
    );
    const both = [entry({ metric_name: 'if_oper_status', description: 'ignore me' })];
    expect(metricOptions(both, LABELS).find((o) => o.name === 'if_oper_status')!.meaning).toBe(
      'Whether the port is up. 1 = up, 2 = down.',
    );
  });

  it('marks a built-in per-interface metric and says nothing about an operator-defined one', () => {
    // Whether a table's rows are interfaces is an OID rule that only Rust knows, so a metric the
    // generated catalog has never heard of gets no badge rather than a guessed one.
    const by = (n: string) => metricOptions(CATALOG, LABELS).find((o) => o.name === n)!;
    expect(by('if_oper_status').perInterface).toBe(true);
    expect(by('cisco_env_temp').perInterface).toBe(false);
    expect(by('acme_widget_temp').perInterface).toBeUndefined();
  });

  it('offers only per-port metrics when the rule is scoped to one interface', () => {
    // An interface-scoped rule on a node-wide metric is structurally inert: the engine passes a
    // port number only for a per-interface metric, and an interface rule with none matches
    // nothing. So the picker offering one is an offer of a rule that saves, lists and never fires.
    const names = metricOptions(CATALOG, LABELS, undefined, { onlyPerInterface: true }).map(
      (o) => o.name,
    );
    expect(names).toContain('if_oper_status');
    // The derived pair states its own dimension — the generated catalogue only knows about
    // metrics that are *collected*, so it could never answer for them.
    expect(names).toContain('if_in_util_pct');
    expect(names).toContain('if_in_bps');
    // Node-wide metrics and Yagra's own checks are gone…
    expect(names).not.toContain('cisco_env_temp');
    expect(names).not.toContain('icmp_rtt_ms');
    // …and so is a metric whose dimension nobody can state. Guessing would be worse: this is how
    // a chassis reading came to be reported per port (ADR-011). The typed-name row is the way in.
    expect(names).not.toContain('acme_widget_temp');
    // Nothing is filtered without the option — every other caller is unaffected.
    expect(metricOptions(CATALOG, LABELS).map((o) => o.name)).toContain('cisco_env_temp');
  });

  it('keeps the value being edited even when the rule is port-scoped', () => {
    // Same trap as below, and the filter must not spring it: a rule on a metric this list would
    // not now offer has to stay editable, or reopening it blanks the control.
    const names = metricOptions(CATALOG, LABELS, 'acme_widget_temp', {
      onlyPerInterface: true,
    }).map((o) => o.name);
    expect(names).toContain('acme_widget_temp');
  });

  it('keeps the value being edited even when the catalog no longer offers it', () => {
    // A control whose value is absent from its options renders blank, and the next save stores
    // something else — the trap the scope-level select already carries a comment about.
    const names = metricOptions(CATALOG, LABELS, 'retired_metric').map((o) => o.name);
    expect(names[0]).toBe('retired_metric');
    // …and does not duplicate one it already has.
    expect(
      metricOptions(CATALOG, LABELS, 'if_oper_status').filter((o) => o.name === 'if_oper_status'),
    ).toHaveLength(1);
  });
});

describe('filterMetricOptions', () => {
  const opts = metricOptions(CATALOG, LABELS);

  it('matches on the metric name', () => {
    expect(filterMetricOptions(opts, 'oper').map((o) => o.name)).toEqual(['if_oper_status']);
    // The derived metrics are searchable by name too, and by the direction in it — which is the
    // whole reason they are two names rather than one (ADR-076).
    expect(filterMetricOptions(opts, 'util_pct').map((o) => o.name)).toEqual([
      'if_in_util_pct',
      'if_out_util_pct',
    ]);
    expect(filterMetricOptions(opts, 'if_out_util').map((o) => o.name)).toEqual([
      'if_out_util_pct',
    ]);
  });

  it('matches on the metric set', () => {
    expect(filterMetricOptions(opts, 'Acme').map((o) => o.name)).toEqual(['acme_widget_temp']);
  });

  it('matches on the meaning, which is often the only place the plain word appears', () => {
    // "temperature" appears in `cisco_env_temp`'s sentence but not in its name.
    expect(filterMetricOptions(opts, 'temperature').map((o) => o.name)).toEqual(['cisco_env_temp']);
  });

  it('returns everything for a blank term', () => {
    expect(filterMetricOptions(opts, '   ')).toHaveLength(opts.length);
  });
});

describe('offersCustomName', () => {
  const opts = metricOptions(CATALOG, LABELS);

  it('offers a name the catalog has never heard of', () => {
    // An operator can attach a collection item with any valid name, and the item writer never
    // consults the catalog — a closed list would block a metric that really exists.
    expect(offersCustomName(opts, 'my_own_metric')).toBe(true);
  });

  it('does not offer a name that is already in the list, or a blank one', () => {
    expect(offersCustomName(opts, 'if_oper_status')).toBe(false);
    expect(offersCustomName(opts, '  ')).toBe(false);
  });
});

describe('groupMetricOptions', () => {
  it('keeps first-seen order and collects every row under its heading', () => {
    const groups = groupMetricOptions(metricOptions(CATALOG, LABELS));
    expect(groups[0].group).toBe("Yagra's own checks");
    expect(groups.map((g) => g.group)).toEqual([
      "Yagra's own checks",
      // The derived metrics sit right after the checks and before anything collected: they are
      // Yagra's own numbers too, and a reader hunting for them under a vendor heading would not
      // find them (ADR-076).
      'Computed by Yagra',
      'Standard SNMP',
      'Cisco IOS/IOS-XE health',
      'Acme',
    ]);
    // No row is lost or duplicated by the grouping.
    expect(groups.reduce((n, g) => n + g.items.length, 0)).toBe(
      metricOptions(CATALOG, LABELS).length,
    );
  });
});
