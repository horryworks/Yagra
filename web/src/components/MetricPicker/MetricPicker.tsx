// SPDX-License-Identifier: AGPL-3.0-only
// Choosing a metric for an alert rule or a mute (ADR-075 増分 4).
//
// It replaces a free-text box whose only help was a five-entry `datalist`, against a catalog of
// eighty-eight. Three things it has to do that a `<select>` could not:
//
//  1. **Be readable before the choice is made.** A threshold on `bgp_peer_state` is `above 3` or
//     `below 3` depending on which of 1–6 means established. So each row carries its sentence, and
//     the search matches the sentence as well as the name — "temperature" finds `cisco_env_temp`.
//  2. **Stay open.** An operator can attach a collection item with any valid metric name, so a
//     closed list would block a metric that really exists. Type a name nothing matches and the
//     last row offers to use it.
//  3. **Not offer what the save will refuse.** Counters are dropped in `metricOptions`.
//
// Judgement lives in `lib/metricCatalog.ts`, not here — Vitest never executes `.tsx`
// (`testing.md`), so a rule written in this file is a rule with no test. This component owns the
// popover, the keyboard, and nothing else.

import { useEffect, useLayoutEffect, useMemo, useRef, useState } from 'react';
import type { KeyboardEvent } from 'react';
import { useTranslation } from 'react-i18next';
import { AnchoredPopover, focusPopoverTrigger } from '../ui/AnchoredPopover';
import { SearchInput } from '../ui/SearchInput';
import { api } from '../../services/api';
import type { MibCatalogEntry } from '../../types/api';
import {
  filterMetricOptions,
  groupMetricOptions,
  metricOptions,
  offersCustomName,
  type MetricOption,
} from '../../lib/metricCatalog';
import './MetricPicker.css';

interface Props {
  /** The metric name currently on the form — the parent owns it. */
  value: string;
  onChange: (metric: string) => void;
  id?: string;
}

export function MetricPicker({ value, onChange, id }: Props) {
  const { t } = useTranslation('metrics');
  const { t: tf } = useTranslation('format');
  const [open, setOpen] = useState(false);
  const [placed, setPlaced] = useState(false);
  const [query, setQuery] = useState('');
  const [catalog, setCatalog] = useState<MibCatalogEntry[]>([]);
  const [active, setActive] = useState(0);
  const wrapRef = useRef<HTMLDivElement>(null);
  const listRef = useRef<HTMLDivElement>(null);

  // Loaded when the popover first opens, not on mount: the dialog this sits in is usually opened
  // to edit a bound, and the catalog is only needed by someone who is changing the metric.
  useEffect(() => {
    if (!open || catalog.length) return undefined;
    let live = true;
    void api
      .listMibCatalog()
      .then((rows) => {
        if (live) setCatalog(rows);
      })
      // A catalog that will not load is not a reason to block the field: the search box still
      // accepts a typed name, which is exactly what this control replaced.
      .catch(() => undefined);
    return () => {
      live = false;
    };
  }, [open, catalog.length]);

  const options = useMemo(
    () =>
      metricOptions(
        catalog,
        {
          checks: t('picker.checks'),
          derived: t('picker.derived'),
          standard: t('picker.standard'),
          liveness: tf('liveness'),
          // The key already carries its namespace (`metrics:meaning.…`), so it resolves whichever
          // namespace this component was mounted under.
          meaning: (key) => t(key, { defaultValue: '' }),
        },
        value,
      ),
    [catalog, t, tf, value],
  );

  const shown = useMemo(() => filterMetricOptions(options, query), [options, query]);
  const custom = offersCustomName(options, query);
  const groups = useMemo(() => groupMetricOptions(shown), [shown]);
  /** Flat order, so ↑/↓ walk the list the way it is drawn rather than the way it is grouped. */
  const flat = useMemo(() => groups.flatMap((g) => g.items), [groups]);

  useEffect(() => setActive(0), [query]);

  // The popover is `visibility: hidden` while it is measured, and a hidden element cannot take
  // focus — so waiting for `onPlacedChange` is what makes this fire at all (ui-conventions.md).
  useLayoutEffect(() => {
    if (open && placed) {
      wrapRef.current?.ownerDocument
        ?.querySelector<HTMLInputElement>('.metricpick-search input')
        ?.focus({ preventScroll: true });
    }
  }, [open, placed]);

  // Keep the highlighted row in view — the list scrolls, and the keyboard is the only way to reach
  // the bottom of a ninety-row catalog without a mouse.
  useEffect(() => {
    listRef.current
      ?.querySelector<HTMLElement>('[data-active="true"]')
      ?.scrollIntoView({ block: 'nearest' });
  }, [active]);

  const dismiss = (restoreFocus: boolean) => {
    setOpen(false);
    setPlaced(false);
    setQuery('');
    if (restoreFocus) focusPopoverTrigger(wrapRef.current, 'listbox');
  };

  const pick = (metric: string) => {
    if (!metric) return;
    onChange(metric);
    dismiss(true);
  };

  const onKeyDown = (e: KeyboardEvent<HTMLDivElement>) => {
    const last = flat.length + (custom ? 1 : 0) - 1;
    if (last < 0) return;
    if (e.key === 'ArrowDown') {
      e.preventDefault();
      setActive((i) => Math.min(last, i + 1));
    } else if (e.key === 'ArrowUp') {
      e.preventDefault();
      setActive((i) => Math.max(0, i - 1));
    } else if (e.key === 'Enter') {
      e.preventDefault();
      pick(active < flat.length ? flat[active].name : query.trim());
    }
  };

  const selected = options.find((o) => o.name === value);

  return (
    <div className="metricpick" ref={wrapRef}>
      <button
        type="button"
        id={id}
        className={`metricpick-trigger${value ? '' : ' placeholder'}`}
        aria-haspopup="listbox"
        aria-expanded={open}
        onClick={() => (open ? dismiss(false) : setOpen(true))}
      >
        <span className="mono">{value ? (selected?.label ?? value) : t('picker.placeholder')}</span>
      </button>
      {/* The sentence for what is already chosen, outside the popover: an operator who opened this
          dialog to change a bound never opens the list, and would otherwise never see it. */}
      {selected?.meaning && <span className="modal-hint">{selected.meaning}</span>}
      <AnchoredPopover
        open={open}
        anchorRef={wrapRef}
        role="listbox"
        label={t('picker.search')}
        align="start"
        className="metricpick-pop"
        onDismiss={dismiss}
        onPlacedChange={setPlaced}
        onKeyDown={onKeyDown}
      >
        <div className="metricpick-search">
          <SearchInput
            value={query}
            onChange={setQuery}
            placeholder={t('picker.search')}
            ariaLabel={t('picker.search')}
          />
        </div>
        <div className="metricpick-list" ref={listRef}>
          {groups.map((g) => (
            <div key={g.group} className="metricpick-group">
              <div className="metricpick-group-head">{g.group}</div>
              {g.items.map((o) => (
                <Row
                  key={o.name}
                  option={o}
                  perInterfaceLabel={t('picker.perInterface')}
                  selected={o.name === value}
                  active={flat.indexOf(o) === active}
                  onPick={() => pick(o.name)}
                />
              ))}
            </div>
          ))}
          {custom && (
            <button
              type="button"
              className="metricpick-row custom"
              data-active={flat.length === active}
              onClick={() => pick(query.trim())}
            >
              <span className="metricpick-name mono">
                {t('picker.custom', { name: query.trim() })}
              </span>
            </button>
          )}
          {!flat.length && !custom && <p className="metricpick-empty">{t('picker.empty')}</p>}
        </div>
        <p className="metricpick-note">{t('picker.scopeNote')}</p>
      </AnchoredPopover>
    </div>
  );
}

function Row({
  option,
  perInterfaceLabel,
  selected,
  active,
  onPick,
}: {
  option: MetricOption;
  perInterfaceLabel: string;
  selected: boolean;
  active: boolean;
  onPick: () => void;
}) {
  return (
    <button
      type="button"
      className={`metricpick-row${selected ? ' selected' : ''}`}
      data-active={active}
      onClick={onPick}
    >
      <span className="metricpick-head">
        <span className="metricpick-name mono">{option.label}</span>
        {option.perInterface && <span className="metricpick-badge">{perInterfaceLabel}</span>}
      </span>
      {/* The meaning if there is one, else where it comes from. Never a filler sentence — an OID
          says something true, "a metric collected from this device" says nothing. */}
      {option.meaning ? (
        <span className="metricpick-meaning">{option.meaning}</span>
      ) : (
        option.oid && <span className="metricpick-meaning mono muted">{option.oid}</span>
      )}
    </button>
  );
}
