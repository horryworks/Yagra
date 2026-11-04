// SPDX-License-Identifier: AGPL-3.0-only
// General-purpose widgets — cards that are about the board's reader rather than about the fleet.
// The world clock (ADR-237) is the first: it reads no API, so it is the same card signed in, on a
// shared board and on the public one.

import { useEffect, useState } from 'react';
import { useTranslation } from 'react-i18next';
import { Button } from '../../components/ui/Button';
import { FieldError, TextInput } from '../../components/ui/Field';
import { IconButton } from '../../components/ui/IconButton';
import { Segmented } from '../../components/ui/Segmented';
import { isImeComposing } from '../../lib/ime';
import { intlLocale } from '../../lib/locale';
import { knownTimeZones, zoneDateText } from '../../lib/timeZone';
import { NothingChosen } from '../primitives/NothingChosen';
import type { WidgetProps } from '../types';
import {
  MAX_ZONES,
  addZone,
  cityName,
  moveZone,
  readClockSettings,
  readZone,
  realTimeZone,
  viewerTimeZone,
  type ClockSettings,
  type ClockZone,
} from './worldClock';

/** The current instant, re-read on each second boundary so every card on the board turns over
 *  together rather than whenever it happened to mount. */
function useNow(): Date {
  const [now, setNow] = useState(() => new Date());
  useEffect(() => {
    let timer: ReturnType<typeof setTimeout>;
    const step = () => {
      const t = new Date();
      setNow(t);
      timer = setTimeout(step, 1000 - (t.getTime() % 1000) + 5);
    };
    timer = setTimeout(step, 1000 - (Date.now() % 1000) + 5);
    return () => clearTimeout(timer);
  }, []);
  return now;
}

function SunIcon() {
  return (
    <svg viewBox="0 0 16 16" fill="none" stroke="currentColor" strokeWidth="1.4" aria-hidden="true">
      <circle cx="8" cy="8" r="3" fill="currentColor" />
      <path d="M8 1.5v1.8M8 12.7v1.8M1.5 8h1.8M12.7 8h1.8M3.4 3.4l1.3 1.3M11.3 11.3l1.3 1.3M3.4 12.6l1.3-1.3M11.3 4.7l1.3-1.3" />
    </svg>
  );
}

function MoonIcon() {
  return (
    <svg viewBox="0 0 16 16" aria-hidden="true">
      <path d="M10.9 2.2a6 6 0 1 0 2.9 8.6A5 5 0 0 1 10.9 2.2Z" fill="currentColor" />
    </svg>
  );
}

function useZoneName() {
  const { t } = useTranslation('dashboard');
  return (z: ClockZone) =>
    z.label.trim() || (cityName(z.tz) ?? t('widgets.worldClock.local'));
}

/** One zone: the face and the line under it. `solo` lays it out as the card's only clock. */
function ZoneFace({
  zone,
  settings,
  now,
  viewerTz,
  solo,
}: {
  zone: ClockZone;
  settings: ClockSettings;
  now: Date;
  viewerTz: string;
  solo: boolean;
}) {
  const { t, i18n } = useTranslation('dashboard');
  const name = useZoneName()(zone);
  const tz = realTimeZone(zone.tz, viewerTz);
  const r = readZone(tz, now, settings.hour12, viewerTz);
  const date = zoneDateText(now.getTime(), tz, intlLocale(i18n.language));
  const shift =
    r.dayShift < 0 ? t('widgets.worldClock.yesterday') : r.dayShift > 0 ? t('widgets.worldClock.tomorrow') : '';
  const icon = (
    <span
      className={`wclock-ico ${r.night ? 'is-night' : 'is-day'}`}
      role="img"
      aria-label={r.night ? t('widgets.worldClock.night') : t('widgets.worldClock.day')}
    >
      {r.night ? <MoonIcon /> : <SunIcon />}
    </span>
  );
  const time = (
    <span className="wclock-time">
      {r.hh}:{r.mm}
      {settings.seconds && <span className="wclock-sec">:{r.ss}</span>}
      {settings.hour12 && <span className="wclock-ampm">{r.dayPeriod}</span>}
    </span>
  );
  const meta = (
    <span className="wclock-meta">
      <span className="wclock-when">
        {date} · {r.offset}
      </span>
      {shift && <span className="wclock-shift">{shift}</span>}
    </span>
  );

  if (solo) {
    return (
      <div className="wclock-solo">
        <span className="wclock-label" title={tz}>
          {icon}
          {name}
        </span>
        {time}
        {meta}
      </div>
    );
  }
  return (
    <li className={`wclock-zone${r.night ? ' is-night' : ''}`}>
      {icon}
      <span className="wclock-name">
        <span className="wclock-label" title={tz}>
          {name}
        </span>
        {meta}
      </span>
      {time}
    </li>
  );
}

/** Digital clocks for the zones the card holds. */
export function WorldClockWidget({ instance, openSettings }: WidgetProps) {
  const { t } = useTranslation('dashboard');
  const settings = readClockSettings(instance.settings);
  const now = useNow();
  const viewerTz = viewerTimeZone();

  if (settings.zones.length === 0) {
    return (
      <NothingChosen
        text={t('widgets.worldClock.none')}
        choose={t('widgets.worldClock.choose')}
        openSettings={openSettings}
      />
    );
  }
  const solo = settings.zones.length === 1;
  return (
    <div className="wclock">
      {solo ? (
        <ZoneFace zone={settings.zones[0]} settings={settings} now={now} viewerTz={viewerTz} solo />
      ) : (
        <ul className="wclock-zones">
          {settings.zones.map((z) => (
            <ZoneFace key={z.tz} zone={z} settings={settings} now={now} viewerTz={viewerTz} solo={false} />
          ))}
        </ul>
      )}
    </div>
  );
}

/** A zone's display name, committed on blur or Enter: every commit persists the layout. */
function LabelInput({ zone, onCommit }: { zone: ClockZone; onCommit: (label: string) => void }) {
  const { t } = useTranslation('dashboard');
  const [draft, setDraft] = useState(zone.label);
  useEffect(() => setDraft(zone.label), [zone.label]);
  const commit = () => {
    if (draft !== zone.label) onCommit(draft);
  };
  return (
    <TextInput
      className="wclock-labelinput"
      value={draft}
      placeholder={cityName(zone.tz) ?? t('widgets.worldClock.local')}
      aria-label={t('widgets.worldClock.labelAria')}
      onChange={(e) => setDraft(e.target.value)}
      onBlur={commit}
      onKeyDown={(e) => {
        if (isImeComposing(e)) return; // `lib/ime.ts`
        if (e.key === 'Enter') {
          e.preventDefault();
          commit();
        }
      }}
    />
  );
}

/** Customize-mode settings: which zones, and how the time is written. */
export function WorldClockSettings({ instance, setSettings }: WidgetProps) {
  const { t } = useTranslation('dashboard');
  const settings = readClockSettings(instance.settings);
  const { zones } = settings;
  const [draft, setDraft] = useState('');
  const [error, setError] = useState<'unknown' | 'duplicate' | null>(null);
  const localName = t('widgets.worldClock.local');
  // Datalists are addressed by a document-global id, so it carries the instance's.
  const listId = `wclock-zones-${instance.instanceId}`;

  const save = (next: ClockZone[]) => setSettings({ zones: next });
  const add = () => {
    const r = addZone(zones, draft, localName);
    if (r.ok) {
      save(r.zones);
      setDraft('');
      setError(null);
    } else if (r.reason !== 'full') {
      setError(r.reason);
    }
  };

  return (
    <div className="wclock-settings">
      <div className="wclock-settings-head">
        <span>{t('widgets.worldClock.zones')}</span>
        <span className="wclock-count">
          {t('widgets.worldClock.count', { n: zones.length, max: MAX_ZONES })}
        </span>
      </div>
      {zones.length > 0 && (
        <ul className="wclock-zonelist">
          {zones.map((z, i) => (
            <li key={z.tz} className="wclock-zonerow">
              <LabelInput
                zone={z}
                onCommit={(label) => save(zones.map((x, j) => (j === i ? { ...x, label } : x)))}
              />
              <span className="wclock-tz mono" title={realTimeZone(z.tz, viewerTimeZone())}>
                {realTimeZone(z.tz, viewerTimeZone())}
              </span>
              <IconButton
                title={t('widgets.worldClock.moveUp')}
                disabled={i === 0}
                onClick={() => save(moveZone(zones, i, -1))}
              >
                ↑
              </IconButton>
              <IconButton
                title={t('widgets.worldClock.moveDown')}
                disabled={i === zones.length - 1}
                onClick={() => save(moveZone(zones, i, 1))}
              >
                ↓
              </IconButton>
              <IconButton
                title={t('widgets.worldClock.remove')}
                danger
                onClick={() => save(zones.filter((_, j) => j !== i))}
              >
                ✕
              </IconButton>
            </li>
          ))}
        </ul>
      )}
      {zones.length < MAX_ZONES && (
        <div className="wclock-add">
          <div className="wclock-addrow">
            <TextInput
              className="mono wclock-addinput"
              list={listId}
              value={draft}
              placeholder="Asia/Tokyo"
              aria-label={t('widgets.worldClock.addAria')}
              aria-invalid={error !== null}
              onChange={(e) => {
                setDraft(e.target.value);
                setError(null);
              }}
              onKeyDown={(e) => {
                if (isImeComposing(e)) return; // `lib/ime.ts`
                if (e.key === 'Enter') {
                  e.preventDefault();
                  add();
                }
              }}
            />
            <datalist id={listId}>
              <option value={localName}>{viewerTimeZone()}</option>
              {knownTimeZones().map((tz) => (
                <option key={tz} value={tz} />
              ))}
            </datalist>
            <Button disabled={draft.trim() === ''} onClick={add}>
              {t('common:actions.add')}
            </Button>
          </div>
          {error && <FieldError>{t(`widgets.worldClock.error.${error}`)}</FieldError>}
        </div>
      )}
      <div className="wclock-settings-head">
        <span>{t('widgets.worldClock.format')}</span>
      </div>
      <div className="wclock-format">
        <Segmented
          size="sm"
          ariaLabel={t('widgets.worldClock.format')}
          value={settings.hour12 ? '12' : '24'}
          onChange={(v) => setSettings({ hour12: v === '12' })}
          options={[
            { value: '24', label: t('widgets.worldClock.h24') },
            { value: '12', label: t('widgets.worldClock.h12') },
          ]}
        />
        <label className="wclock-check">
          <input
            type="checkbox"
            checked={settings.seconds}
            onChange={(e) => setSettings({ seconds: e.target.checked })}
          />
          {t('widgets.worldClock.seconds')}
        </label>
      </div>
    </div>
  );
}
