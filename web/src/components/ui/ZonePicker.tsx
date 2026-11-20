// SPDX-License-Identifier: AGPL-3.0-only
// The time zone a form's times are typed in, changed from the zone's name beside the time
// (ADR-261 decision 6): `EST ▾` opens a panel with the operator's own zone, the target's site zone,
// the zones used lately, UTC, and a search over every zone (`new york`, `utc+5`).
//
// A zone is never typed into the time box itself (decision 8): `CST` names three different clocks,
// and a silent wrong guess is a window thirteen hours off.

import { useLayoutEffect, useMemo, useRef, useState } from 'react';
import { useTranslation } from 'react-i18next';
import { AnchoredPopover } from './AnchoredPopover';
import { SearchField } from './SearchField';
import { knownTimeZones, utcOffsetText, zoneAbbreviation } from '../../lib/timeZone';
import { matchZones } from '../../lib/maintenanceTime';
import './ZonePicker.css';

interface Props {
  /** The zone in force. */
  zone: string;
  onChange: (zone: string) => void;
  /** The instant the abbreviation and offsets are read at (the window's start), so `EST` and
   *  `EDT` are named right for the date being typed. */
  at: number;
  /** The operator's display zone. */
  mine: string;
  /** The zone the target's site keeps, or `null` when it has none (or its nodes disagree). */
  site: string | null;
  /** Zones picked lately, newest first. */
  recent: readonly string[];
}

interface Row {
  zone: string;
  /** Why this zone is offered, for the short word before it. */
  tag?: 'mine' | 'site' | 'recent';
}

export function ZonePicker({ zone, onChange, at, mine, site, recent }: Props) {
  const { t } = useTranslation('common');
  const anchor = useRef<HTMLSpanElement>(null);
  const search = useRef<HTMLInputElement>(null);
  const [open, setOpen] = useState(false);
  const [placed, setPlaced] = useState(false);
  const [query, setQuery] = useState('');
  const zones = useMemo(() => knownTimeZones(), []);

  useLayoutEffect(() => {
    if (open && placed) search.current?.focus({ preventScroll: true });
  }, [open, placed]);

  const quick: Row[] = useMemo(() => {
    const rows: Row[] = [{ zone: mine, tag: 'mine' }];
    if (site) rows.push({ zone: site, tag: 'site' });
    for (const z of recent) rows.push({ zone: z, tag: 'recent' });
    rows.push({ zone: 'UTC' });
    const seen = new Set<string>();
    return rows.filter((r) => !seen.has(r.zone) && seen.add(r.zone));
  }, [mine, site, recent]);

  const matches = useMemo(
    () => (query.trim() ? matchZones(query, zones, at) : []),
    [query, zones, at],
  );

  const close = () => {
    setOpen(false);
    setQuery('');
  };
  const pick = (z: string) => {
    onChange(z);
    close();
  };

  const row = (r: Row) => (
    <button
      key={`${r.tag ?? ''}${r.zone}`}
      type="button"
      className="zp-row"
      aria-pressed={r.zone === zone}
      onClick={() => pick(r.zone)}
    >
      {r.tag && <span className="zp-row-tag">{t(`zonePicker.tag.${r.tag}`)}</span>}
      <span className="zp-row-name">{r.zone}</span>
      <span className="zp-row-abbr">
        {zoneAbbreviation(r.zone, at)} · UTC{utcOffsetText(at, r.zone)}
      </span>
    </button>
  );

  return (
    <span ref={anchor} className="zp">
      <button
        type="button"
        className="zp-tag"
        aria-haspopup="dialog"
        aria-expanded={open}
        aria-label={t('zonePicker.change', { zone })}
        title={zone}
        onClick={() => (open ? close() : setOpen(true))}
      >
        {zoneAbbreviation(zone, at)} <span aria-hidden="true">▾</span>
      </button>
      <AnchoredPopover
        open={open}
        anchorRef={anchor}
        role="dialog"
        label={t('zonePicker.label')}
        align="end"
        className="zp-panel"
        onPlacedChange={setPlaced}
        onDismiss={close}
      >
        <SearchField
          inputRef={search}
          value={query}
          onChange={(e) => setQuery(e.target.value)}
          onClear={() => setQuery('')}
          placeholder="new york, utc+5, tokyo"
          aria-label={t('zonePicker.search')}
        />
        <div className="zp-list">
          {query.trim() ? (
            matches.length > 0 ? (
              matches.map((z) => row({ zone: z }))
            ) : (
              <p className="zp-empty">{t('zonePicker.none')}</p>
            )
          ) : (
            quick.map(row)
          )}
        </div>
      </AnchoredPopover>
    </span>
  );
}
