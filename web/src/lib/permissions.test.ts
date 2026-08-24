// SPDX-License-Identifier: AGPL-3.0-only
// ADR-056 Inc.2: the WebUI decides "may I do X?" from the server's matrix, and nowhere else.
//
// Two things are under test. The lookup itself — which must fail *closed* on every unknown — and
// the guard at the bottom, which reads `src/**/*.tsx` and fails when a write control is drawn from
// `authed` or from a role comparison. That guard is the increment's whole point: the 35 sites were
// individually reasonable, and it is only the *rule* that stops the 36th.
import { describe, expect, it } from 'vitest';
import { readdirSync, readFileSync } from 'node:fs';
import { join } from 'node:path';
import { grants, permissionLabel } from './permissions';
import { releasePermission, releasableRows, type SuppressionPanelRow } from './suppression';
import type { RoleMatrix } from '../types/api';

const MATRIX: RoleMatrix = {
  permissions: [
    { key: 'view', label: 'View', description: 'View inventory.' },
    { key: 'ack_alerts', label: 'Respond to incidents', description: 'Ack or mute.' },
    { key: 'manage_maintenance', label: 'Manage maintenance', description: 'Windows.' },
    { key: 'manage_config', label: 'Manage configuration', description: 'Config.' },
  ],
  roles: [
    { key: 'viewer', label: 'Viewer', description: '', builtin: true, permissions: ['view'] },
    {
      key: 'operator',
      label: 'Operator',
      description: '',
      builtin: true,
      permissions: ['view', 'ack_alerts', 'manage_maintenance'],
    },
    {
      key: 'admin',
      label: 'Admin',
      description: '',
      builtin: true,
      permissions: ['view', 'ack_alerts', 'manage_maintenance', 'manage_config'],
    },
  ],
};

describe('grants', () => {
  it('answers from the matrix, per role', () => {
    expect(grants(MATRIX, 'viewer', 'view')).toBe(true);
    expect(grants(MATRIX, 'viewer', 'ack_alerts')).toBe(false);
    expect(grants(MATRIX, 'operator', 'manage_maintenance')).toBe(true);
    expect(grants(MATRIX, 'operator', 'manage_config')).toBe(false);
    expect(grants(MATRIX, 'admin', 'manage_config')).toBe(true);
  });

  // Each of these is a moment the UI actually passes through: the first render after a reload, the
  // signed-out state, and a token naming a role a newer server invented. All three must answer no —
  // the opposite mistake draws a control that 403s.
  it('fails closed on everything it does not know', () => {
    expect(grants(null, 'admin', 'manage_config')).toBe(false);
    expect(grants(MATRIX, null, 'view')).toBe(false);
    expect(grants(MATRIX, 'auditor', 'view')).toBe(false);
  });
});

describe('permissionLabel', () => {
  it('uses the server catalogue', () => {
    expect(permissionLabel(MATRIX, 'manage_maintenance')).toBe('Manage maintenance');
  });

  it('falls back to the key before the matrix arrives', () => {
    expect(permissionLabel(null, 'manage_config')).toBe('manage_config');
  });
});

describe('release actions follow what they release', () => {
  const row = (kind: 'maintenance' | 'mute'): SuppressionPanelRow => ({
    key: kind,
    kind,
    headKey: 'h',
    title: 'w',
    endsAt: '2026-01-01T00:00:00Z',
    action: { action: { action: 'end-window', windowId: 'w1' }, labelKey: 'l' },
  });

  it('maps a window to maintenance and a mute to incident response', () => {
    expect(releasePermission('maintenance')).toBe('manage_maintenance');
    expect(releasePermission('mute')).toBe('ack_alerts');
  });

  it('removes only the control the caller may not use, and keeps the explanation', () => {
    const can = (p: string) => p === 'manage_maintenance';
    const out = releasableRows([row('maintenance'), row('mute')], (p) => can(p));
    expect(out[0].action).not.toBeNull();
    expect(out[1].action).toBeNull();
    // The block itself survives — a viewer still has to be able to read why the node is silent.
    expect(out).toHaveLength(2);
    expect(out[1].title).toBe('w');
  });
});

// ---------------------------------------------------------------------------------------------
// The guard.
// ---------------------------------------------------------------------------------------------

/** Every `.tsx` under `src/`. */
function tsxFiles(dir: string, out: string[] = []): string[] {
  for (const e of readdirSync(dir, { withFileTypes: true })) {
    const p = join(dir, e.name);
    if (e.isDirectory()) tsxFiles(p, out);
    else if (e.name.endsWith('.tsx')) out.push(p);
  }
  return out;
}

const SRC = join(__dirname, '..');

/**
 * Needles assembled at runtime, or this file would match itself and fail forever — the same trap
 * `loadState.test.ts` and `reports/guards.rs::the_run_state_sql_is_built_from_the_enum` document.
 */
const A = `${'auth'}${'ed'}`;

/**
 * The spellings that drew a control from "am I signed in", each one a shape that actually shipped.
 *
 * Not a blanket ban on the word: a screen may still ask whether anyone is signed in, and several
 * legitimately do — a fetch that should not run when signed out, and the "sign in to continue" pane
 * that replaces a whole screen. Those are about *authentication* and stay. What is banned is
 * `authed` reaching a control, because "signed in" is not "permitted": every `+ Add`, edit and
 * delete listed here was drawn for a Viewer and then refused by the server.
 *
 * The list has **no exemptions**, which is the point — an exemption table is where the next one
 * would go to be forgotten. If a new legitimate spelling appears, it belongs here with its reason,
 * not in a list of files that are allowed to be wrong.
 */
const CONTROL_FROM_SESSION: { re: RegExp; why: string }[] = [
  {
    re: new RegExp(`disabled=\\{!${A}\\b`),
    why: 'greys a control out for anyone signed out — and leaves it live for a Viewer who may not use it. A control nobody may use is not drawn.',
  },
  {
    re: new RegExp(`\\{${A} && `),
    why: 'draws a control when signed in. Ask for the permission the action needs.',
  },
  {
    re: new RegExp(`=\\{${A}\\}`),
    why: 'passes "signed in" as a capability prop (canEdit={authed}).',
  },
  {
    re: new RegExp(`=\\{${A} \\? `),
    why: 'hands a write callback to a child when signed in, rather than when permitted.',
  },
];

/** How a component reads *the signed-in principal's* role — as opposed to a row's role, which is
 *  data and may be compared freely (a badge tone, a picker value). */
const PRINCIPAL_ROLE = /useAuthStore\(\s*\(s\)\s*=>\s*s\.role\s*\)/;
const ROLE_COMPARISON = new RegExp(
  `\\brole\\s*[!=]==\\s*['"](${['admin', 'operator', 'viewer'].join('|')})['"]`,
);

describe('write controls are drawn from a permission, not from a role or a session', () => {
  const files = tsxFiles(SRC);

  it('finds the sources it is supposed to be reading', () => {
    // Without this, a wrong path makes every assertion below vacuously true — the failure mode a
    // guard cannot have, because it looks exactly like success.
    expect(files.length).toBeGreaterThan(100);
    expect(
      files.filter((f) => readFileSync(f, 'utf8').includes('useCan(')).length,
    ).toBeGreaterThan(10);
  });

  it('no component compares the signed-in role to a role name', () => {
    // `role === 'admin'` is a second copy of `rbac.rs`'s matrix, in a language nothing compares to
    // the first. Five components held one; the Shared dashboard's even rendered as a *disabled*
    // button with a hover tooltip, which a phone cannot show at all.
    const offenders = files.filter((f) => {
      const src = readFileSync(f, 'utf8');
      return PRINCIPAL_ROLE.test(src) && ROLE_COMPARISON.test(src);
    });
    expect(offenders.map((f) => f.slice(SRC.length + 1).replace(/\\/g, '/'))).toEqual([]);
  });

  it('no component draws a control from being signed in', () => {
    const offenders: string[] = [];
    for (const f of files) {
      const src = readFileSync(f, 'utf8');
      for (const { re, why } of CONTROL_FROM_SESSION) {
        if (re.test(src)) offenders.push(`${f.slice(SRC.length + 1).replace(/\\/g, '/')}: ${why}`);
      }
    }
    expect(offenders).toEqual([]);
  });
});
