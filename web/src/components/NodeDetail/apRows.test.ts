// SPDX-License-Identifier: AGPL-3.0-only
// The judgement behind the AP tab (ADR-064 増分 B3). Every case here is one an operator hits on a
// real controller: an AP nobody named, an HA pair reporting the same AP twice, a scoped operator
// who can see the standby but not the active, and an inventory cut off at the cap.

import { readFileSync } from 'node:fs';
import { join } from 'node:path';
import { describe, expect, it } from 'vitest';
import type { WirelessApRow, WirelessApSighting, WirelessControllerSummary } from '../../types/api';
import {
  apLabel,
  apSearchText,
  apStateKey,
  apsOverCap,
  awaitingFirstInventory,
  isImported,
  reportingControllers,
  MAX_APS_DEFAULT,
  MAX_APS_HARD,
} from './apRows';

const ACTIVE = '11111111-1111-4111-8111-111111111111';
const STANDBY = '22222222-2222-4222-8222-222222222222';

function sighting(controller: string | null, over: Partial<WirelessApSighting> = {}): WirelessApSighting {
  return {
    controller_node_id: controller,
    state: 'associated',
    run_state: 'normal',
    clients: 3,
    last_seen: '2026-09-18T00:00:00Z',
    last_associated_at: null,
    ...over,
  };
}

function row(over: Partial<WirelessApRow> = {}): WirelessApRow {
  return {
    ap_id: '33333333-3333-4333-8333-333333333333',
    mac: 'a0:b1:c2:d3:e4:f5',
    name: 'AP-101',
    serial: null,
    model: 'AP4050DN',
    sw_version: null,
    ip: '10.0.0.9',
    vendor_group: null,
    state: 'associated',
    run_state: 'normal',
    clients: 3,
    node_id: null,
    controller_node_id: ACTIVE,
    first_seen: '2026-09-18T00:00:00Z',
    last_seen: '2026-09-18T00:00:00Z',
    last_associated_at: null,
    reported_by: [sighting(ACTIVE)],
    ...over,
  };
}

function summary(over: Partial<WirelessControllerSummary> = {}): WirelessControllerSummary {
  return {
    node_id: ACTIVE,
    flavor: 'huawei',
    aps_reported: 30,
    aps_truncated_at: null,
    last_inventory_at: '2026-09-18T00:00:00Z',
    import_aps: true,
    max_aps: 1024,
    ap_group_id: null,
    aps_over_cap: 0,
    ...over,
  };
}

describe('apStateKey', () => {
  it('passes a known state through', () => {
    expect(apStateKey(row({ state: 'backup' }))).toBe('backup');
    expect(apStateKey(row({ state: 'not_associated' }))).toBe('not_associated');
  });

  it('calls an absent state unknown rather than rendering it blank', () => {
    // `state` is null both for a token this build does not know and for an inventory row that
    // carried none. Neither should leave the cell empty — an empty cell reads as "no problem".
    expect(apStateKey(row({ state: null }))).toBe('unknown');
  });
});

describe('isImported', () => {
  it('is exactly whether the AP has a node', () => {
    expect(isImported(row({ node_id: null }))).toBe(false);
    expect(isImported(row({ node_id: '44444444-4444-4444-8444-444444444444' }))).toBe(true);
  });

  it('does not read the state — an associated AP is not thereby monitored', () => {
    // The mistake this pins: "associated" is about the radio, not about Yagra. An AC reports 30
    // associated APs on a deployment that has imported none of them.
    expect(isImported(row({ state: 'associated', node_id: null }))).toBe(false);
    expect(isImported(row({ state: 'not_associated', node_id: 'x' }))).toBe(true);
  });
});

describe('apLabel', () => {
  it('prefers the name', () => {
    expect(apLabel(row({ name: 'Floor-3-East' }))).toBe('Floor-3-East');
  });

  it('falls back to the MAC when the AP was never named', () => {
    expect(apLabel(row({ name: null }))).toBe('a0:b1:c2:d3:e4:f5');
    expect(apLabel(row({ name: '   ' }))).toBe('a0:b1:c2:d3:e4:f5');
  });
});

describe('reportingControllers', () => {
  it('names the serving controller and no other when only one reports it', () => {
    expect(reportingControllers(row())).toEqual({ serving: ACTIVE, others: [] });
  });

  it('puts the HA peer in `others` — the case the column exists for', () => {
    const r = row({ reported_by: [sighting(ACTIVE), sighting(STANDBY, { clients: 0 })] });
    expect(reportingControllers(r)).toEqual({ serving: ACTIVE, others: [STANDBY] });
  });

  it('never repeats a controller that reported twice', () => {
    const r = row({ reported_by: [sighting(ACTIVE), sighting(ACTIVE)] });
    expect(reportingControllers(r).others).toEqual([]);
  });

  it('drops a sighting whose controller the caller cannot see', () => {
    const r = row({ reported_by: [sighting(ACTIVE), sighting(null)] });
    expect(reportingControllers(r)).toEqual({ serving: ACTIVE, others: [] });
  });

  it('reports the others even when the serving controller is out of scope', () => {
    // A scoped operator who can see the standby but not the active: `controller_node_id` is blanked
    // and `reported_by` keeps what they may see. "No controller" would be a lie — one is serving it.
    const r = row({ controller_node_id: null, reported_by: [sighting(STANDBY)] });
    expect(reportingControllers(r)).toEqual({ serving: null, others: [STANDBY] });
  });

  it('is empty for an AP no controller has reported in service', () => {
    expect(reportingControllers(row({ controller_node_id: null, reported_by: [] }))).toEqual({
      serving: null,
      others: [],
    });
  });
});

describe('apSearchText', () => {
  it('finds the MAC typed with and without separators', () => {
    const text = apSearchText(row());
    expect(text).toContain('a0:b1:c2:d3:e4:f5');
    expect(text).toContain('a0b1c2d3e4f5');
  });

  it('covers every field the row puts on screen', () => {
    const text = apSearchText(row({ serial: 'SN123', sw_version: 'V200R019' })).join(' ');
    for (const needle of ['AP-101', '10.0.0.9', 'AP4050DN', 'SN123', 'V200R019']) {
      expect(text).toContain(needle);
    }
  });

  it('answers with empty strings rather than `null` for the fields an AP may not have', () => {
    expect(apSearchText(row({ name: null, ip: null, model: null })).join('')).not.toContain('null');
  });
});

describe('the controller summary', () => {
  it('counts what the cap dropped', () => {
    expect(apsOverCap(summary({ aps_over_cap: 7 }))).toBe(7);
    expect(apsOverCap(summary())).toBe(0);
    expect(apsOverCap(null)).toBe(0);
  });

  it('tells "no APs" apart from "no inventory yet"', () => {
    // Both are zero rows on screen, and they mean opposite things: one is a controller with nothing
    // behind it, the other is one we have not heard from.
    expect(awaitingFirstInventory(summary({ last_inventory_at: null }))).toBe(true);
    expect(awaitingFirstInventory(summary())).toBe(false);
    expect(awaitingFirstInventory(null)).toBe(false);
  });
});

// `apRows.ts` holds a second copy of two backend constants because the API does not serve them
// (its doc says why). A copy with no check drifts silently — and `MAX_APS_HARD` is the page size
// the tab fetches, so a smaller copy would show a first page as the whole inventory. Read as text,
// the way `lib/nodeKind.test.ts` reads `node_kind.rs`.
describe('the AP caps match yagra_common::wlan', () => {
  const src = readFileSync(
    join(__dirname, '..', '..', '..', '..', 'crates', 'yagra-common', 'src', 'wlan.rs'),
    'utf8',
  );
  const rustConst = (name: string): number | undefined => {
    const m = new RegExp(`^pub const ${name}: u32 = (\\d+);$`, 'm').exec(src);
    return m ? Number(m[1]) : undefined;
  };

  it('finds both constants in the Rust source', () => {
    expect(rustConst('MAX_APS_PER_CONTROLLER_DEFAULT')).toBeTypeOf('number');
    expect(rustConst('MAX_APS_PER_CONTROLLER_HARD')).toBeTypeOf('number');
    expect(rustConst('NOT_A_REAL_CONST')).toBeUndefined();
  });

  it('agrees on the default and the hard cap', () => {
    expect(MAX_APS_DEFAULT).toBe(rustConst('MAX_APS_PER_CONTROLLER_DEFAULT'));
    expect(MAX_APS_HARD).toBe(rustConst('MAX_APS_PER_CONTROLLER_HARD'));
  });
});
