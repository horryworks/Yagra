// SPDX-License-Identifier: AGPL-3.0-only
// Nodes ▸ Subnet overlaps (ADR-187) — the screen's judgement, where a node-environment test runs.
// `SubnetOverlapsPage.tsx` is layout plus the calls (`testing.md`).
//
// The comparison itself is the server's (`crates/yagra-core/src/subnet_overlaps.rs`). What is
// decided here is only how the answer is shown: which tab an overlap sits on, what a hint suggests
// as a rule, where each range sits on the bar, and what an empty table means.

import type { TFunction } from 'i18next';
import type { ColumnFilterSpec } from '../lib/columnFilter';
import type {
  ExclusionReason,
  OverlapKind,
  OverlapPlace,
  OverlapRuleBody,
  OverlapStatus,
  SubnetOverlap,
  SubnetOverlapsView,
} from '../types/api';
import { OVERLAP_KINDS } from '../types/api';

/** The overlaps on one tab, optionally narrowed to one kind. The server already orders them. */
export function overlapsOn(
  view: SubnetOverlapsView | null,
  tab: OverlapStatus,
  kind: OverlapKind | null,
): SubnetOverlap[] {
  if (!view) return [];
  return view.overlaps.filter((o) => o.status === tab && (kind === null || o.kind === kind));
}

/** How many open overlaps there are of each kind — the three tiles above the table. */
export function openKindCounts(view: SubnetOverlapsView | null): Record<OverlapKind, number> {
  const counts = Object.fromEntries(OVERLAP_KINDS.map((k) => [k, 0])) as Record<OverlapKind, number>;
  for (const o of view?.overlaps ?? []) if (o.status === 'open') counts[o.kind] += 1;
  return counts;
}

/** A place's site, as a stable key (`null` is the tree root). */
const siteKey = (p: OverlapPlace) => p.site_id ?? '';

/** The sites an overlap's visible places name, in the order the server listed them. A site with no
 *  name (the tree root, or a folder deleted since) carries `name: null` and the caller words it. */
export function visibleSites(o: SubnetOverlap): { key: string; name: string | null }[] {
  const seen = new Map<string, string | null>();
  for (const p of o.places) if (!seen.has(siteKey(p))) seen.set(siteKey(p), p.site_name ?? null);
  return [...seen].map(([key, name]) => ({ key, name }));
}

/**
 * The Subnet overlaps filter row, keyed by `Column.key`. Only the site column filters: the range,
 * kind and count columns already have the tiles and the tabs. The column's hint is where "what a
 * site is" is said (it used to be the page note, ADR-200 Inc.12).
 */
export function overlapFilters(t: TFunction): Record<string, ColumnFilterSpec<SubnetOverlap>> {
  return {
    sites: {
      kind: 'text',
      modes: ['contains', 'regex'],
      not: true,
      readText: (o) => visibleSites(o).map((s) => s.name ?? t('subnetOverlaps.root')),
      containsSemantics: 'substring',
      placeholder: t('subnetOverlaps.cols.sites'),
      hint: t('subnetOverlaps.sitesFilterHint'),
    },
  };
}

/** A port name or description split into lowercase words — `subnet_overlaps.rs::words_of`. */
function wordsOf(text: string): string[] {
  return text
    .toLowerCase()
    .split(/[^a-z0-9]+/)
    .filter((w) => w !== '');
}

/** Whether one word of a port's text is `word`, optionally followed by digits (`dialer1`). */
function wordIs(token: string, word: string): boolean {
  return token === word || token.replace(/[0-9]+$/, '') === word;
}

/**
 * Whether a rule's port text appears in `text` as whole words, in order, ignoring case.
 *
 * ⚠️ A mirror of `crates/yagra-core/src/subnet_overlaps.rs::carries_words`, which decides what a
 * rule actually excludes. This copy only previews it in the add dialog; nothing compares the two,
 * so a change there must be made here too (the cases below are the Rust tests' own).
 */
export function carriesWords(text: string, needle: string): boolean {
  const wanted = wordsOf(needle);
  if (wanted.length === 0) {
    const plain = needle.trim().toLowerCase();
    return plain !== '' && text.toLowerCase().includes(plain);
  }
  const words = wordsOf(text);
  for (let i = 0; i + wanted.length <= words.length; i++) {
    if (wanted.every((w, j) => wordIs(words[i + j], w))) return true;
  }
  return false;
}

/** How many ports on the loaded list a rule's port text would take, and the first few by name.
 *  `null` while the text is empty — the field then says nothing. One port is one (node, ifindex),
 *  however many of its addresses the list carries. */
export function portMatches(
  places: readonly OverlapPlace[],
  portText: string,
  shown = 3,
): { count: number; names: string[] } | null {
  if (portText.trim() === '') return null;
  const seen = new Set<string>();
  const names: string[] = [];
  for (const p of places) {
    const key = `${p.node_id}/${p.ifindex}`;
    if (seen.has(key)) continue;
    const on = (s: string | null | undefined) => !!s && carriesWords(s, portText);
    if (!on(p.if_name) && !on(p.if_alias)) continue;
    seen.add(key);
    if (names.length < shown) {
      const port = p.if_name ?? `#${p.ifindex}`;
      names.push(p.node_name ? `${p.node_name} ${port}` : port);
    }
  }
  return { count: seen.size, names };
}

/** The rule a hint suggests, pre-filled for the add dialog, or `null` when the hint names no word
 *  (a shared line or a template is recognised by its shape, which no rule can express). */
export function suggestedRule(o: SubnetOverlap): OverlapRuleBody | null {
  const hint = o.hint;
  if (!hint) return null;
  switch (hint.kind) {
    case 'wan':
      return { port_text: hint.word, reason: 'wan', note: '', enabled: true };
    case 'redundancy':
      return { port_text: hint.word, reason: 'redundancy', note: '', enabled: true };
    case 'shared_line':
    case 'template':
      return null;
    default: {
      const never: never = hint;
      return never;
    }
  }
}

/** What the add-rule dialog holds while it is being filled in. */
export interface RuleDraft {
  range: string;
  portText: string;
  reason: ExclusionReason;
  note: string;
}

/** The dialog's starting state: blank, or a hint's suggestion. */
export function draftFrom(body: OverlapRuleBody | null): RuleDraft {
  return {
    range: body?.range ?? '',
    portText: body?.port_text ?? '',
    reason: body?.reason ?? 'wan',
    note: body?.note ?? '',
  };
}

/** The body the draft sends, or the key of the reason it cannot be sent yet. The range is checked
 *  only for shape here; the server parses it and answers `invalid_range` for the rest. */
export function ruleBody(
  d: RuleDraft,
): { body: OverlapRuleBody } | { refuse: 'rules.form.needSomething' | 'rules.form.badRange' } {
  const range = d.range.trim();
  const portText = d.portText.trim();
  if (!range && !portText) return { refuse: 'rules.form.needSomething' };
  if (range && !/^[0-9A-Fa-f:.]+\/\d{1,3}$/.test(range)) return { refuse: 'rules.form.badRange' };
  return {
    body: {
      range: range || null,
      port_text: portText || null,
      reason: d.reason,
      note: d.note.trim(),
      enabled: true,
    },
  };
}

/** The same rule with its switch flipped — the whole body, because the endpoint replaces. */
export function toggled(rule: {
  range?: string | null;
  port_text?: string | null;
  reason: ExclusionReason;
  note: string;
  enabled: boolean;
}): OverlapRuleBody {
  return {
    range: rule.range ?? null,
    port_text: rule.port_text ?? null,
    reason: rule.reason,
    note: rule.note,
    enabled: !rule.enabled,
  };
}

/** One bar under a nested overlap: where a range sits inside the outer one, in percent. */
export interface RangeBar {
  subnet: string;
  leftPct: number;
  widthPct: number;
  outer: boolean;
}

function v4Start(cidr: string): { start: number; size: number } | null {
  const m = /^(\d+)\.(\d+)\.(\d+)\.(\d+)\/(\d+)$/.exec(cidr);
  if (!m) return null;
  const octets = m.slice(1, 5).map(Number);
  const len = Number(m[5]);
  if (octets.some((o) => o > 255) || len > 32) return null;
  const start = octets.reduce((a, o) => a * 256 + o, 0);
  return { start, size: 2 ** (32 - len) };
}

/** The bars for a nested overlap: the outer range full width, each inner one where it sits.
 *  IPv4 only — a /64 inside a /48 is 1/65536 of the bar and says nothing a list does not. */
export function rangeBars(o: SubnetOverlap): RangeBar[] {
  if (o.kind !== 'nested') return [];
  const outer = v4Start(o.subnet);
  if (!outer) return [];
  const bars: RangeBar[] = [{ subnet: o.subnet, leftPct: 0, widthPct: 100, outer: true }];
  for (const inner of o.inner) {
    const r = v4Start(inner);
    if (!r) continue;
    bars.push({
      subnet: inner,
      leftPct: ((r.start - outer.start) / outer.size) * 100,
      widthPct: (r.size / outer.size) * 100,
      outer: false,
    });
  }
  return bars;
}

/** Which empty-state sentence a tab shows. "Nothing overlaps" is only said when every device the
 *  caller can see has reported its addresses — otherwise the honest sentence names the gap. */
export function emptyKey(
  view: SubnetOverlapsView | null,
  tab: OverlapStatus,
): { key: string; missing: number } {
  const missing = view ? Math.max(0, view.nodes_total - view.nodes_with_addresses) : 0;
  if (tab !== 'open') return { key: `empty.${tab}`, missing };
  if (!view || view.nodes_with_addresses === 0) return { key: 'empty.noAddresses', missing };
  return { key: missing > 0 ? 'empty.openPartial' : 'empty.open', missing };
}
