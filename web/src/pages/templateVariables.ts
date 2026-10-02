// SPDX-License-Identifier: AGPL-3.0-only
// How the notification-template editor groups and orders the variables a template may use
// (ADR-039 Inc.2).
//
// The variables themselves are the server's (`GET /api/v1/notification-channels/template-variables`,
// from `yagra_common::TEMPLATE_VARIABLES`), and so is whether each one is on every alert. What lives
// here is only what an operator reads: the order, the group each name sits under, and - through the
// `routing.template.vars.*` locale keys - a name and an explanation in the operator's language.
//
// This list is a second copy of the server's names, so it is pinned to it from the Rust side:
// `api/notifications.rs::every_template_variable_is_one_the_editor_groups` reads the array below and
// compares it with `TEMPLATE_VARIABLES` in both directions. Keep `TEMPLATE_VARIABLE_NAMES` a plain
// array of quoted names; that test reads it as text.

/** The groups the insert list shows, in order. */
export const TEMPLATE_VARIABLE_GROUPS = [
  'subject',
  'what',
  'numbers',
  'time',
  'upstream',
  'ids',
] as const;
export type TemplateVariableGroup = (typeof TEMPLATE_VARIABLE_GROUPS)[number];

/** Every variable, in the order the insert list shows them. */
export const TEMPLATE_VARIABLE_NAMES = [
  'subject_name',
  'node_name',
  'node_address',
  'group',
  'profile',
  'tags',
  'subject_kind',
  'event',
  'severity',
  'state',
  'flapping',
  'metric',
  'value',
  'threshold',
  'direction',
  'ifindex',
  'row_name',
  'at',
  'at_unix_ms',
  'root_cause_name',
  'root_cause_id',
  'node_id',
  'check_id',
  'dedup_key',
] as const;
export type TemplateVariableName = (typeof TEMPLATE_VARIABLE_NAMES)[number];

/** Which group each variable is listed under. */
export const TEMPLATE_VARIABLE_GROUP_OF: Readonly<Record<TemplateVariableName, TemplateVariableGroup>> =
  {
    subject_name: 'subject',
    node_name: 'subject',
    node_address: 'subject',
    group: 'subject',
    profile: 'subject',
    tags: 'subject',
    subject_kind: 'subject',
    event: 'what',
    severity: 'what',
    state: 'what',
    flapping: 'what',
    metric: 'numbers',
    value: 'numbers',
    threshold: 'numbers',
    direction: 'numbers',
    ifindex: 'numbers',
    row_name: 'numbers',
    at: 'time',
    at_unix_ms: 'time',
    root_cause_name: 'upstream',
    root_cause_id: 'upstream',
    node_id: 'ids',
    check_id: 'ids',
    dedup_key: 'ids',
  };

/** Whether a name is one this editor knows. */
export function isTemplateVariable(name: string): name is TemplateVariableName {
  return (TEMPLATE_VARIABLE_NAMES as readonly string[]).includes(name);
}

/** One row of the insert list. */
export interface InsertListGroup {
  group: TemplateVariableGroup;
  names: TemplateVariableName[];
}

/**
 * The insert list, narrowed to what matches `query`.
 *
 * `labelOf` and `descriptionOf` are the operator's-language strings, so a Japanese operator can type
 * the Japanese name. The variable's own name always matches too, for the operator who already knows
 * it. Groups left empty by the query are dropped rather than drawn as a bare heading.
 */
export function insertList(
  query: string,
  labelOf: (name: TemplateVariableName) => string,
  descriptionOf: (name: TemplateVariableName) => string,
): InsertListGroup[] {
  const q = query.trim().toLowerCase();
  const matches = (n: TemplateVariableName) =>
    q === '' ||
    n.includes(q) ||
    labelOf(n).toLowerCase().includes(q) ||
    descriptionOf(n).toLowerCase().includes(q);
  return TEMPLATE_VARIABLE_GROUPS.map((group) => ({
    group,
    names: TEMPLATE_VARIABLE_NAMES.filter((n) => TEMPLATE_VARIABLE_GROUP_OF[n] === group && matches(n)),
  })).filter((g) => g.names.length > 0);
}
