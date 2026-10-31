// SPDX-License-Identifier: AGPL-3.0-only
// The top-bar clock (ADR-221 decision 3): the time now in the display zone, and the one place that
// zone is chosen. The first row follows the browser; any other row pins a zone for this account.
// A chosen zone puts a dot on the clock, so a screen read in a zone other than the browser's says
// so without a sentence. Above the list sits a world map to choose from (Inc.2); a phone has no
// room for it and lists the common zones instead.

import { useEffect, useLayoutEffect, useRef, useState } from 'react';
import { useTranslation } from 'react-i18next';
import { usePrefsStore } from '../../prefs';
import { setTimeZone } from '../../serverPrefs';
import { AnchoredPopover } from '../ui/AnchoredPopover';
import { SearchField } from '../ui/SearchField';
import { TEST_IDS } from '../../testIds';
// The screen's refresh cadence: a minute display redrawn at it is never more than that late.
import { POLL_INTERVAL_MS } from '../../lib/sharedPoll';
import { browserTimeZone, clockText, displayTimeZone, zoneAbbreviation } from '../../lib/timeZone';
import { useIsMobileViewport } from '../../lib/viewport';
import { initialZoneRows, matchTimeZones, zoneCountry } from '../../lib/zoneMap';
import { ZoneMap } from './ZoneMap';
import './ZoneClock.css';

export function ZoneClock() {
  const { t, i18n } = useTranslation('nav');
  const lang = i18n.language;
  const phone = useIsMobileViewport();
  // Read so a change redraws this component; the value itself is applied by `App.tsx`.
  const chosen = usePrefsStore((s) => s.timeZone);
  const [now, setNow] = useState(() => Date.now());
  const [open, setOpen] = useState(false);
  const [query, setQuery] = useState('');
  // Focus moves into the search box once the popover is placed: it is hidden while measured, and
  // a hidden element cannot take focus (ui-conventions, AnchoredPopover).
  const [placed, setPlaced] = useState(false);
  const ref = useRef<HTMLDivElement>(null);
  const searchRef = useRef<HTMLInputElement>(null);

  useEffect(() => {
    const id = window.setInterval(() => setNow(Date.now()), POLL_INTERVAL_MS);
    return () => window.clearInterval(id);
  }, []);

  useLayoutEffect(() => {
    if (open && placed) searchRef.current?.focus({ preventScroll: true });
  }, [open, placed]);

  const zone = displayTimeZone();
  const pinned = Boolean(chosen);
  const browser = browserTimeZone();
  const matches = initialZoneRows(matchTimeZones(query, lang), query, phone, pinned ? zone : null);

  const pick = (z: string) => {
    setTimeZone(z);
    setOpen(false);
    setQuery('');
  };

  return (
    <div className="zclock" ref={ref}>
      <button
        className="zclock-btn"
        onClick={() => setOpen((o) => !o)}
        aria-label={t('shell.timeZone')}
        aria-haspopup="dialog"
        aria-expanded={open}
      >
        <span className="zclock-time">{clockText(now)}</span>
        <span className="zclock-zone">{zoneAbbreviation(zone, now)}</span>
        {pinned && <span className="zclock-dot" data-testid={TEST_IDS.zoneClockDot} aria-hidden />}
      </button>
      <AnchoredPopover
        open={open}
        anchorRef={ref}
        role="dialog"
        label={t('shell.timeZone')}
        align="end"
        className={phone ? 'zclock-pop' : 'zclock-pop has-map'}
        onDismiss={() => {
          setOpen(false);
          setQuery('');
        }}
        onPlacedChange={setPlaced}
      >
        <SearchField
          inputRef={searchRef}
          boxClassName="zclock-searchbox"
          className="zclock-search"
          value={query}
          onChange={(e) => setQuery(e.target.value)}
          onClear={() => setQuery('')}
          placeholder={t('shell.timeZoneSearch')}
          aria-label={t('shell.timeZone')}
        />
        {!phone && <ZoneMap zone={zone} now={now} query={query} lang={lang} onPick={pick} />}
        <ul className="zclock-list">
          <li>
            <button
              className={pinned ? 'zclock-item' : 'zclock-item is-current'}
              onClick={() => pick('')}
            >
              <span>{t('shell.followBrowser', { zone: browser })}</span>
              <span className="zclock-abbr">{zoneAbbreviation(browser, now)}</span>
            </button>
          </li>
          {matches.length > 0 && <li className="zclock-sep" aria-hidden />}
          {matches.map((z) => (
            <li key={z}>
              <button
                className={pinned && z === zone ? 'zclock-item is-current' : 'zclock-item'}
                onClick={() => pick(z)}
              >
                <span>
                  {z}
                  {zoneCountry(z, lang) && (
                    <span className="zclock-country">{zoneCountry(z, lang)}</span>
                  )}
                </span>
                <span className="zclock-abbr">{zoneAbbreviation(z, now)}</span>
              </button>
            </li>
          ))}
          {matches.length === 0 && query.trim() !== '' && (
            <li className="zclock-empty">{t('shell.noTimeZone')}</li>
          )}
        </ul>
      </AnchoredPopover>
    </div>
  );
}
