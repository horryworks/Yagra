// SPDX-License-Identifier: AGPL-3.0-only
// Which fields "Edit node" offers for a node's kind, and what the two saves it makes contain.
//
// The regression this exists to prevent: one dialog rendered the same five fields — device profile,
// SNMP credential, maker, model, pool — for every `NodeKind`, having never read `node.kind`. For a
// URL or DNS monitor an SNMP credential is not "usually blank", it is structurally absent:
// `scheduler.rs::assemble_node_jobs` returns one HTTP/DNS job before the SNMP branch, so
// `node.credential_id` is written by the dialog and read by nothing. The backend neither refuses
// nor warns — it stores the binding, counts it in the credential's `used_by`, and never looks at it
// again. The read side already knew this (`overviewFacts.ts` drops those rows for url/dns); only
// the edit side was left behind.
//
// In a `.ts` because Vitest never runs `.tsx` (`testing.md`). The registry and the request builder
// live together on purpose: the invariant that matters is a relation *between* them — a field this
// kind hides is still sent — and splitting them makes two halves of one rule that nothing pins.

import { api } from '../../services/api';
import {
  NODE_KINDS,
  type DnsCheckConfig,
  type NodeDetail,
  type NodeKind,
  type ProfileSummary,
  type UrlCheckConfig,
} from '../../types/api';
import {
  dnsBodyFrom,
  dnsDraftFrom,
  urlBodyFrom,
  urlDraftFrom,
  type CheckFormProblem,
  type DnsCheckDraft,
  type UrlCheckDraft,
} from './checkConfigForm';

/** Every row the dialog can show, in the order it lays them out. What the node is monitored *for*
 *  comes before how the node itself is set up: an operator opens this to change the check, not the
 *  folder. One token is one row of the form — `identity` is the Maker/Model pair, which share a
 *  line and are never useful one without the other. */
export const NODE_EDIT_FIELDS = [
  'urlCheck',
  'dnsCheck',
  'profile',
  'snmpCredential',
  'identity',
  'pool',
] as const;

export type NodeEditField = (typeof NODE_EDIT_FIELDS)[number];

/** Which half of the dialog a row belongs to. The dialog heads each half when a kind has both;
 *  a kind with one section gets no heading, because there is nothing to tell apart.
 *
 *  An `as const` array because the dialog builds `editNode.section.${s}` at runtime, and a member
 *  with no strings renders that raw key in both locales (`extensibility.md` §4). */
export const NODE_EDIT_SECTIONS = ['check', 'node'] as const;
export type NodeEditSection = (typeof NODE_EDIT_SECTIONS)[number];

/** Kinds that are walked over SNMP and therefore report their own maker/model. A Meraki node keeps
 *  the descriptive pair — the org collector writes `'Cisco Meraki'` + the model on the node's one
 *  INSERT (`meraki.rs`, no `ON CONFLICT` and no `UPDATE nodes` anywhere in that file), so a hand
 *  edit is never overwritten — but not the credential: its org API key lives on `meraki_orgs` and
 *  the scheduler short-circuits Meraki before per-node job assembly. */
const DESCRIBED_BY_A_DEVICE: readonly NodeKind[] = ['device', 'meraki'];

/** Membership keyed by *field*, the shape `tabs.ts` argues for: a `Record<NodeKind, Field[]>` would
 *  make adding one field four row edits with a silent failure if you miss one. The opposite
 *  direction — a fifth `NodeKind` — is covered by the verbatim per-kind pins in the test. */
export const NODE_EDIT_FIELD_META: Record<
  NodeEditField,
  { kinds: readonly NodeKind[]; section: NodeEditSection }
> = {
  urlCheck: { kinds: ['url'], section: 'check' },
  dnsCheck: { kinds: ['dns'], section: 'check' },
  // Meaningful for every kind, though it means less for a monitor: a URL node never resolves a
  // collection set, so its profile carries only the poll interval and the inherited thresholds.
  profile: { kinds: NODE_KINDS, section: 'node' },
  // The one that caused this file. `resolve_snmp_auth` is the only reader of `node.credential_id`
  // and is unreachable for the other three kinds. Note the URL form carries its own credential
  // picker (`http_auth`/`api_token`) — showing this one too would put two in a single dialog.
  snmpCredential: { kinds: ['device'], section: 'node' },
  identity: { kinds: DESCRIBED_BY_A_DEVICE, section: 'node' },
  pool: { kinds: NODE_KINDS, section: 'node' },
};

/** Facts owned by the kind rather than by a field. */
export interface NodeEditKindSpec {
  /** `nodes`-namespace dialog title. Reuses the strings the check dialogs already had. */
  readonly titleKey: string;
  /** What to call the profile picker. "Device profile" is wrong on a URL monitor — nothing about it
   *  is a device, and the profile only supplies its interval and thresholds. */
  readonly profileLabelKey: string;
  /** The one `ProfileCategory` this kind's picker offers; `null` ⇒ every category that is not some
   *  other kind's (i.e. the device classes). Tokens mirror `lib/profileCategories.ts`. */
  readonly profileCategory: string | null;
}

export const NODE_EDIT_KIND_SPEC: Record<NodeKind, NodeEditKindSpec> = {
  device: {
    titleKey: 'detail.editNode',
    profileLabelKey: 'field.deviceProfile',
    profileCategory: null,
  },
  meraki: {
    titleKey: 'detail.editNode',
    profileLabelKey: 'field.deviceProfile',
    profileCategory: null,
  },
  url: {
    titleKey: 'checkEdit.urlTitle',
    profileLabelKey: 'field.monitorProfile',
    profileCategory: 'url-check',
  },
  dns: {
    titleKey: 'checkEdit.dnsTitle',
    profileLabelKey: 'field.monitorProfile',
    profileCategory: 'dns-check',
  },
};

/** Categories that belong to an endpoint monitor, and are therefore *not* offered to a device.
 *  Derived, so a fifth kind with its own category drops out of the device picker without a second
 *  list being edited. */
const MONITOR_CATEGORIES: readonly string[] = NODE_KINDS.map(
  (k) => NODE_EDIT_KIND_SPEC[k].profileCategory,
).filter((c): c is string => c !== null);

/** The fields this kind shows, in `NODE_EDIT_FIELDS` order. Derived from the registry so no kind
 *  can reshuffle the dialog relative to another. */
export function visibleNodeEditFields(kind: NodeKind): readonly NodeEditField[] {
  return NODE_EDIT_FIELDS.filter((f) => NODE_EDIT_FIELD_META[f].kinds.includes(kind));
}

/** The sections this kind shows, in field order. One section ⇒ the dialog draws no headings: a
 *  device's five fields are all "the node", and heading a form that has only one part is noise. */
export function visibleNodeEditSections(kind: NodeKind): readonly NodeEditSection[] {
  const seen: NodeEditSection[] = [];
  for (const f of visibleNodeEditFields(kind)) {
    const s = NODE_EDIT_FIELD_META[f].section;
    if (!seen.includes(s)) seen.push(s);
  }
  return seen;
}

/** The profiles this kind's picker offers, in the caller's order.
 *
 *  ⚠️ The node's **current** profile is kept even when it falls outside the filter. A controlled
 *  `<select>` whose value matches no option renders blank while the state still holds the id, so
 *  the operator would see an empty picker and any save would look like a deliberate re-bind. */
export function profileOptions(
  kind: NodeKind,
  all: readonly ProfileSummary[],
  currentId: string,
): ProfileSummary[] {
  const want = NODE_EDIT_KIND_SPEC[kind].profileCategory;
  return all.filter(
    (p) =>
      (want === null ? !MONITOR_CATEGORIES.includes(p.category) : p.category === want) ||
      (currentId !== '' && p.id === currentId),
  );
}

/** Whether the bound profile is one this kind's filter would otherwise hide — the dialog says so
 *  rather than showing a `dns-check` node a lone `router` entry with no explanation. */
export function profileIsOffKind(
  kind: NodeKind,
  all: readonly ProfileSummary[],
  currentId: string,
): boolean {
  if (currentId === '') return false;
  const current = all.find((p) => p.id === currentId);
  if (!current) return false;
  const want = NODE_EDIT_KIND_SPEC[kind].profileCategory;
  return want === null ? MONITOR_CATEGORIES.includes(current.category) : current.category !== want;
}

/** The dialog's fields, as the inputs hold them. `url`/`dns` are non-null only for the kind that
 *  carries that check row. */
export interface NodeEditDraft {
  profileId: string;
  credentialId: string;
  vendor: string;
  model: string;
  pool: string;
  url: UrlCheckDraft | null;
  dns: DnsCheckDraft | null;
}

/** Seed the draft from the node — **including the fields this kind hides**.
 *
 *  That is what makes a hidden field survive a save: it is in the draft, so it is in the body, so
 *  the `PUT` writes it back as it was. `PUT /nodes/{id}/bindings` replaces profile/credential/
 *  vendor/model unconditionally and reads an omitted field as `null`, so hiding one and dropping it
 *  from the payload would silently blank it. Hiding is a *render* decision here, never a payload
 *  one. */
export function nodeEditDraftFrom(node: NodeDetail): NodeEditDraft {
  return {
    profileId: node.profile_id ?? '',
    credentialId: node.credential_id ?? '',
    vendor: node.vendor ?? '',
    model: node.model ?? '',
    pool: node.pool ?? '',
    url: node.url_check ? urlDraftFrom(node.url_check) : null,
    dns: node.dns_check ? dnsDraftFrom(node.dns_check) : null,
  };
}

/** The bindings half of the save. Mirrors `api.setNodeBindings`'s body. */
export interface NodeEditBindings {
  profile_id: string | null;
  credential_id: string | null;
  vendor: string | null;
  model: string | null;
  /** Always a string, never null: `''` clears the assignment back to inherited, while a JSON `null`
   *  reads server-side as "leave unchanged" and would silently drop the edit. */
  pool: string;
}

/** Everything one Save writes. `check` is null for the kinds that have no check row of their own. */
export interface NodeEditRequest {
  check: { kind: 'url'; body: UrlCheckConfig } | { kind: 'dns'; body: DnsCheckConfig } | null;
  bindings: NodeEditBindings;
}

/** The draft → what to send, or the first reason it cannot be sent.
 *
 *  Deliberately does not take the node: the draft is the single source for every field, hidden ones
 *  included. Passing the node as well would give the same value two origins that can disagree. */
export function nodeEditRequest(
  kind: NodeKind,
  d: NodeEditDraft,
): { req: NodeEditRequest } | { error: CheckFormProblem } {
  let check: NodeEditRequest['check'] = null;
  if (kind === 'url' && d.url) {
    const built = urlBodyFrom(d.url);
    if ('error' in built) return { error: built.error };
    check = { kind: 'url', body: built.body };
  } else if (kind === 'dns' && d.dns) {
    const built = dnsBodyFrom(d.dns);
    if ('error' in built) return { error: built.error };
    check = { kind: 'dns', body: built.body };
  }
  return {
    req: {
      check,
      bindings: {
        profile_id: d.profileId || null,
        credential_id: d.credentialId || null,
        vendor: d.vendor.trim() || null,
        model: d.model.trim() || null,
        pool: d.pool.trim(),
      },
    },
  };
}

/** Which half of the save was being made when it failed. */
export type NodeEditStage = 'check' | 'bindings';

export type NodeEditOutcome = { ok: true } | { ok: false; stage: NodeEditStage; error: unknown };

/** Send both halves, check first.
 *
 *  Order is load-bearing. The check `PUT` is the one the *server* can still refuse after the form's
 *  own validation passed — a private/loopback target (SSRF) or a credential without TLS — so
 *  putting it first means a rejection leaves nothing written and the operator sees one coherent
 *  error. Bindings-first would move the pool and then report a failure that is only half true.
 *
 *  Returns the failure instead of throwing so the caller can say *which* half landed. There is no
 *  compensating write and no "already saved" flag: both calls are full replacements and therefore
 *  idempotent, so pressing Save again rewrites the identical check row and retries the bindings —
 *  one redundant write in exchange for no extra state to keep correct. */
export async function sendNodeEdit(
  nodeId: string,
  req: NodeEditRequest,
): Promise<NodeEditOutcome> {
  if (req.check) {
    try {
      if (req.check.kind === 'url') await api.setUrlCheck(nodeId, req.check.body);
      else await api.setDnsCheck(nodeId, req.check.body);
    } catch (error) {
      return { ok: false, stage: 'check', error };
    }
  }
  try {
    await api.setNodeBindings(nodeId, req.bindings);
  } catch (error) {
    return { ok: false, stage: 'bindings', error };
  }
  return { ok: true };
}

/** The one message that reports a *partial* save, and therefore the one that interpolates the
 *  server's reason rather than replacing the whole sentence with it. Exported so the dialog can
 *  recognise it without matching on a string literal. */
export const PARTIAL_SAVE_KEY = 'editNode.err.bindingsAfterCheck';

/** The `nodes`-namespace message for a failed save.
 *
 *  The partial case earns its own sentence: when the check saved and the bindings did not, "failed
 *  to save node" is a lie about the half that landed, and an operator who reopens the dialog would
 *  see their URL change already applied with no idea the pool move was not. */
export function nodeEditErrorKey(req: NodeEditRequest, stage: NodeEditStage): string {
  if (stage === 'check') return 'checkEdit.err.save';
  return req.check ? PARTIAL_SAVE_KEY : 'err.saveNode';
}
