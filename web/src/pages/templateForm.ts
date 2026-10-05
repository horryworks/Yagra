// SPDX-License-Identifier: AGPL-3.0-only
// What the notification-template editor shows for each kind of channel (ADR-197 Inc.2): which
// fields there are, what each one becomes on the wire, and the JSON helpers the JSON kinds need.
//
// The fields follow what the channel actually sends, read from `alerts/notify.rs`:
// - JSM sends the subject as the alert's `message` and the body as its `description`.
// - Email sends a subject line and a body.
// - A webhook POSTs the body and nothing else. The subject reaches no one, so it has no field.
// - PagerDuty sends the subject as `payload.summary` and the body as `custom_details`, and only
//   when an alert fires: a recovery or a roll-up sends a `resolve` with the dedup key alone.
//
// A `.ts` so the judgement is reachable from a test (`testing.md`).

import type { ChannelKind, NotifyEvent } from '../types/api';

export interface TemplateForm {
  /** What the subject field is called, or `null` when the channel sends no subject. */
  subject: 'title' | 'subject' | 'summary' | null;
  /** What the body field is called. */
  body: 'text' | 'json' | 'customDetails';
  /** Whether the body is JSON. */
  json: boolean;
  /** The points in an alert's life at which the template is not used at all. */
  unusedAt: readonly NotifyEvent[];
}

export const TEMPLATE_FORMS: Record<ChannelKind, TemplateForm> = {
  jsm: { subject: 'title', body: 'text', json: false, unusedAt: [] },
  email: { subject: 'subject', body: 'text', json: false, unusedAt: [] },
  webhook: { subject: null, body: 'json', json: true, unusedAt: [] },
  pagerduty: { subject: 'summary', body: 'customDetails', json: true, unusedAt: ['resolve', 'suppress'] },
};

/** The keys of Yagra's built-in JSON body, in the order it writes them (`yagra_alert::Alert`).
 *  Each has an EN/JA sentence under `routing.template.jsonKeys.*`. */
export const BUILTIN_JSON_KEYS = [
  'node',
  'check',
  'severity',
  'state',
  'at_unix_ms',
  'root_cause',
  'flapping',
  'metric',
  'breach',
  'ifindex',
  'row',
  'row_name',
] as const;
export type BuiltinJsonKey = (typeof BUILTIN_JSON_KEYS)[number];

/**
 * The "Start from a JSON skeleton" template (decision 11): the facts a receiver usually wants, each
 * through `tojson` so a quote in a node's name cannot break the document. It is not the built-in
 * JSON - that is the alert itself, written out, and is no template - and the button says so.
 */
export const JSON_SKELETON = [
  '{',
  '  "node": {{ node_name | tojson }},',
  '  "address": {{ node_address | default("") | tojson }},',
  '  "alert": {{ title | default("") | tojson }},',
  '  "event": {{ event | tojson }},',
  '  "state": {{ state | tojson }},',
  '  "severity": {{ severity | tojson }},',
  '  "at": {{ at | tojson }},',
  '  "dedup_key": {{ dedup_key | tojson }}',
  '}',
].join('\n');

export type JsonToken = { kind: 'key' | 'string' | 'number' | 'literal' | 'punct' | 'space'; text: string };

const TOKEN = /\s+|"(?:[^"\\]|\\.)*"|-?\d+(?:\.\d+)?(?:[eE][+-]?\d+)?|true|false|null|[{}[\],:]/gy;

/**
 * A JSON document laid out one value per line, as pieces to colour; `null` when the text is not
 * JSON. The pieces are the document's own characters re-indented, never a re-serialization: a
 * `90.0` stays `90.0`, which `JSON.stringify(JSON.parse(…))` would turn into `90`.
 */
export function prettyJson(text: string): JsonToken[] | null {
  try {
    JSON.parse(text);
  } catch {
    return null;
  }
  const raw: string[] = [];
  TOKEN.lastIndex = 0;
  let m: RegExpExecArray | null;
  while ((m = TOKEN.exec(text)) !== null) {
    if (!/^\s+$/.test(m[0])) raw.push(m[0]);
    if (TOKEN.lastIndex >= text.length) break;
  }
  const out: JsonToken[] = [];
  let depth = 0;
  const newline = () => out.push({ kind: 'space', text: '\n' + '  '.repeat(depth) });
  for (let i = 0; i < raw.length; i++) {
    const t = raw[i];
    const next = raw[i + 1];
    if (t === '{' || t === '[') {
      out.push({ kind: 'punct', text: t });
      // An empty object or array stays on one line.
      if (next === (t === '{' ? '}' : ']')) {
        out.push({ kind: 'punct', text: next });
        i++;
        continue;
      }
      depth++;
      newline();
    } else if (t === '}' || t === ']') {
      depth--;
      newline();
      out.push({ kind: 'punct', text: t });
    } else if (t === ',') {
      out.push({ kind: 'punct', text: t });
      newline();
    } else if (t === ':') {
      out.push({ kind: 'punct', text: ': ' });
    } else if (t.startsWith('"')) {
      out.push({ kind: next === ':' ? 'key' : 'string', text: t });
    } else if (t === 'true' || t === 'false' || t === 'null') {
      out.push({ kind: 'literal', text: t });
    } else {
      out.push({ kind: 'number', text: t });
    }
  }
  return out;
}
