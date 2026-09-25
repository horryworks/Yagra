// SPDX-License-Identifier: AGPL-3.0-only
// The inventory tree's one filter button, and the chips that say what it has set (ADR-177).
//
// Everything that narrows or reshapes the tree sits behind this one button: Pinned only (ADR-146),
// Needs attention (ADR-163), Hide empty folders (ADR-159's "With nodes", renamed) and the State /
// Kind / Pool sets (ADR-053 Inc.6). Before ADR-177 they were four look-alike buttons in a row under
// the pane head, plus a third row the Filter button opened — and every one of them pushed the tree
// down.
//
// ⚠️ **Nothing here holds state.** The two account switches are the prefs store's, the columns are
// the URL's, and Needs attention is a State selection — all owned by `NodesPage`, which hands them
// in with their setters. That is what kept ADR-177 from touching how any of them is saved.
//
// 🚨 **The chip row is not decoration.** With the controls folded away, it is the only place on
// screen that says the tree is narrowed. Which chips it shows is `inventoryChips` in
// `inventoryFilters.ts`, where a test reaches it.

import { useRef, useState } from 'react';
import type { ReactNode } from 'react';
import { useTranslation } from 'react-i18next';
import { AnchoredPopover } from '../components/ui/AnchoredPopover';
import { focusPopoverTrigger } from '../components/ui/focusPopoverTrigger';
import { Button } from '../components/ui/Button';
import { ClearFilters } from '../components/ui/ClearFilters';
import { FunnelIcon, PinIcon } from '../components/ui/icons';
import {
  decodeSet,
  toggleSetValue,
  type FilterState,
  type FilterableColumn,
} from '../lib/columnFilter';
import { inventoryChips, type InventoryChip } from './inventoryFilters';
import './InventoryFilterMenu.css';

export interface InventoryFilterProps {
  columns: readonly FilterableColumn<never>[];
  /** Plain-text names for the three columns. */
  labels: Record<string, string>;
  filters: FilterState;
  onChange: (next: FilterState) => void;
  /** Whether the pins have loaded. A core without the endpoint gets no Pinned only switch. */
  pinsReady: boolean;
  pinnedOnly: boolean;
  onPinnedOnly: (on: boolean) => void;
  attentionOnly: boolean;
  /** Toggles the preset — the same handler the header's "N need attention" count calls. */
  onAttention: () => void;
  hideEmpty: boolean;
  onHideEmpty: (on: boolean) => void;
  /** Clears everything above, and the search term, in one URL write. */
  onClearAll: () => void;
  /** Whether the search box is narrowing too — counted by "Clear all filters". */
  searching: boolean;
}

function Switch({
  checked,
  onChange,
  label,
  hint,
  icon,
}: {
  checked: boolean;
  onChange: (on: boolean) => void;
  label: string;
  hint: string;
  icon?: ReactNode;
}) {
  return (
    <label className="invf-sw" title={hint}>
      <input
        type="checkbox"
        role="switch"
        checked={checked}
        onChange={(e) => onChange(e.target.checked)}
      />
      <span className="invf-sw-track" aria-hidden="true" />
      {icon}
      <span>{label}</span>
    </label>
  );
}

/** The funnel in the pane head, and the popover it opens. */
export function InventoryFilterButton(p: InventoryFilterProps) {
  const { t } = useTranslation('nodes');
  const [open, setOpen] = useState(false);
  const anchorRef = useRef<HTMLSpanElement>(null);
  const count = inventoryChips(p.columns, p.filters, {
    pinnedOnly: p.pinnedOnly,
    hideEmpty: p.hideEmpty,
  }).length;

  const dismiss = (restoreFocus: boolean) => {
    setOpen(false);
    if (restoreFocus) focusPopoverTrigger(anchorRef.current, 'dialog');
  };

  // ⚠️ Focus stays on the trigger when the popover opens. It holds only choices and no text entry,
  // and moving focus onto a switch is one Space away from flipping it (ui-conventions.md).

  const label = t('inventory.filterMenu.label');
  return (
    <span ref={anchorRef} className="invf-anchor">
      <button
        type="button"
        className={count > 0 ? 'nodes-pane-add invf-trigger on' : 'nodes-pane-add invf-trigger'}
        aria-haspopup="dialog"
        aria-expanded={open}
        aria-label={count > 0 ? t('inventory.filterMenu.labelCount', { count }) : label}
        title={label}
        onClick={() => setOpen((o) => !o)}
      >
        <FunnelIcon />
        {count > 0 && <span className="invf-count">{count}</span>}
      </button>
      <AnchoredPopover
        open={open}
        anchorRef={anchorRef}
        role="dialog"
        label={label}
        align="end"
        className="invf-pop"
        onDismiss={dismiss}
      >
        <div className="invf">
          <div className="invf-sec">
            <div className="invf-h">{t('inventory.filterMenu.quick')}</div>
            {p.pinsReady && (
              <Switch
                checked={p.pinnedOnly}
                onChange={p.onPinnedOnly}
                label={t('inventory.pinnedOnly')}
                hint={t('inventory.pinnedOnlyHint')}
                icon={<PinIcon className="invf-pin" />}
              />
            )}
            <Switch
              checked={p.attentionOnly}
              onChange={() => p.onAttention()}
              label={t('inventory.needAttentionOnly')}
              hint={t('inventory.needAttentionOnlyHint')}
            />
            <Switch
              checked={p.hideEmpty}
              onChange={p.onHideEmpty}
              label={t('inventory.withNodesOnly')}
              hint={t('inventory.withNodesOnlyHint')}
            />
          </div>
          {p.columns.map((c) => {
            if (c.filter.kind !== 'enum' || c.filter.options.length === 0) return null;
            const order = c.filter.options.map((o) => o.value);
            const chosen = new Set(decodeSet(p.filters[c.key] ?? ''));
            return (
              <fieldset key={c.key} className="invf-sec">
                <legend className="invf-h">{p.labels[c.key] ?? c.key}</legend>
                <div className="invf-opts">
                  {c.filter.options.map((o) => (
                    <label key={o.value} className="invf-opt">
                      <input
                        type="checkbox"
                        checked={chosen.has(o.value)}
                        onChange={() =>
                          p.onChange({
                            ...p.filters,
                            [c.key]: toggleSetValue(p.filters[c.key] ?? '', o.value, order),
                          })
                        }
                      />
                      <span>{o.label}</span>
                    </label>
                  ))}
                </div>
              </fieldset>
            );
          })}
          <div className="invf-foot">
            <button
              type="button"
              className="invf-clear"
              disabled={count === 0 && !p.searching}
              onClick={p.onClearAll}
            >
              {t('inventory.filterMenu.clearAll')}
            </button>
            <Button onClick={() => dismiss(true)}>{t('inventory.filterMenu.done')}</Button>
          </div>
        </div>
      </AnchoredPopover>
    </span>
  );
}

/** The row under the pane head that says what is in force. Draws nothing when nothing is.
 *
 *  ⚠️ The search term gets no chip — the box above already shows it — but it still keeps the row
 *  (and so `ClearFilters`) on screen, because "clear all filters" clears it too. */
export function InventoryFilterChips(p: InventoryFilterProps) {
  const { t } = useTranslation('nodes');
  const chips = inventoryChips(p.columns, p.filters, {
    pinnedOnly: p.pinnedOnly,
    hideEmpty: p.hideEmpty,
  });
  if (chips.length === 0 && !p.searching) return null;

  const text = (c: InventoryChip): string => {
    switch (c.kind) {
      case 'pinned':
        return t('inventory.pinnedOnly');
      case 'attention':
        return t('inventory.needAttentionOnly');
      case 'hideEmpty':
        return t('inventory.chips.emptyHidden');
      case 'column': {
        const spec = p.columns.find((x) => x.key === c.key)?.filter;
        // A value the list does not offer (a pool named in a link opened before the pools arrived
        // — `readInventoryFilters` keeps those on purpose) shows as its raw token.
        const name = (v: string) =>
          (spec?.kind === 'enum' ? spec.options.find((o) => o.value === v)?.label : undefined) ?? v;
        return `${p.labels[c.key] ?? c.key}: ${c.values.map(name).join(', ')}`;
      }
      default: {
        const never: never = c;
        return never;
      }
    }
  };
  const remove = (c: InventoryChip) => {
    switch (c.kind) {
      case 'pinned':
        return p.onPinnedOnly(false);
      case 'attention':
        return p.onAttention();
      case 'hideEmpty':
        return p.onHideEmpty(false);
      case 'column':
        return p.onChange({ ...p.filters, [c.key]: '' });
      default: {
        const never: never = c;
        return never;
      }
    }
  };

  return (
    <div
      className="nodes-pane-filters invf-chips"
      role="group"
      aria-label={t('inventory.chips.label')}
    >
      {chips.map((c) => {
        const name = text(c);
        return (
          <span
            key={c.kind === 'column' ? `column:${c.key}` : c.kind}
            className={c.kind === 'hideEmpty' ? 'invf-chip soft' : 'invf-chip'}
          >
            <span className="invf-chip-text">{name}</span>
            <button
              type="button"
              className="invf-chip-x"
              aria-label={t('inventory.chips.remove', { name })}
              title={t('inventory.chips.remove', { name })}
              onClick={() => remove(c)}
            >
              ✕
            </button>
          </span>
        );
      })}
      <ClearFilters
        columns={p.columns}
        filters={p.filters}
        extraActive={p.searching || p.pinnedOnly || p.hideEmpty}
        onClear={p.onClearAll}
      />
    </div>
  );
}
