// SPDX-License-Identifier: AGPL-3.0-only
// The forwarding-destination modal's form shape, and the two mappings between it and the API.
//
// Extracted from ForwardingPage.tsx so the mappings are testable (Vitest never runs `.tsx`). They
// are worth reaching: a destination relays log bodies — which routinely carry credentials — off
// box, and the round trip encodes three different "blank means" rules. A secret is never returned
// by the API, so a blank secret box means *keep what is stored*; a CA certificate is not a secret,
// so a blank one means *remove it*; and an omitted service-account key on a new destination
// selects Workload Identity rather than storing a credential at all.

import type {
  ForwardCondition,
  ForwardDestination,
  ForwardDestinationInput,
  ForwardDestKind,
  ForwardSourceKind,
} from '../types/api';
import { usesCommunity, usesServiceAccount, usesTls } from './forwardingOptions';

/** A condition as the form holds it. `Condition.value` is `#[serde(default)]` on the Rust side, so
 *  the contract types it optional even though every response carries it — and the editor's text box
 *  has to stay controlled, which an absent value would break. */
export type DraftCondition = ForwardCondition & { value: string };

/** The editable shape of the modal form — flattened so the condition rows are easy to splice. */
export interface Draft {
  name: string;
  enabled: boolean;
  source_kind: ForwardSourceKind;
  dest_kind: ForwardDestKind;
  target: string;
  pool: string;
  verbatim: boolean;
  mode: 'all' | 'any';
  conditions: DraftCondition[];
  rate_limit: string;
  community: string;
  ca_cert: string;
  service_account_json: string;
}

export function emptyDraft(): Draft {
  return {
    name: '',
    enabled: true,
    source_kind: 'syslog',
    dest_kind: 'syslog_udp',
    target: '',
    pool: '',
    verbatim: true,
    mode: 'all',
    conditions: [],
    rate_limit: '',
    community: '',
    ca_cert: '',
    service_account_json: '',
  };
}

export function draftFrom(row: ForwardDestination): Draft {
  return {
    name: row.name,
    enabled: row.enabled,
    source_kind: row.source_kind,
    dest_kind: row.dest_kind,
    target: row.target,
    pool: row.pool ?? '',
    verbatim: row.verbatim,
    mode: row.filter?.mode ?? 'all',
    conditions: (row.filter?.conditions ?? []).map((c) => ({ ...c, value: c.value ?? '' })),
    rate_limit: row.rate_limit_per_sec == null ? '' : String(row.rate_limit_per_sec),
    community: '',
    ca_cert: row.ca_cert ?? '',
    // Secrets never come back from the API; a blank box means "keep what is stored".
    service_account_json: '',
  };
}

export function toInput(d: Draft): ForwardDestinationInput {
  const rate = d.rate_limit.trim();
  return {
    name: d.name.trim(),
    enabled: d.enabled,
    source_kind: d.source_kind,
    dest_kind: d.dest_kind,
    target: d.target.trim(),
    pool: d.pool.trim() || null,
    verbatim: d.verbatim,
    filter: { mode: d.mode, conditions: d.conditions },
    rate_limit_per_sec: rate ? Number(rate) : null,
    // Always sent (unlike the community): a CA certificate is not a secret, so the form holds its
    // current value and clearing the box has to remove it.
    ca_cert: usesTls(d.dest_kind) ? d.ca_cert.trim() || null : null,
    // Omitted rather than blank: core keeps the stored community when the field is absent.
    ...(usesCommunity(d.dest_kind) && d.community.trim()
      ? { community: d.community.trim() }
      : {}),
    // Same rule for the Google key — and on a *new* destination, omitting it is meaningful: it
    // selects Workload Identity rather than storing a credential.
    ...(usesServiceAccount(d.dest_kind) && d.service_account_json.trim()
      ? { service_account_json: d.service_account_json.trim() }
      : {}),
  };
}

