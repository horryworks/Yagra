// SPDX-License-Identifier: AGPL-3.0-only
// What each table in the WebUI is called, for the things that have to remember something about one
// (ADR-129: the operator's column widths).
//
// WHY A REGISTRY RATHER THAN A FREE STRING. The id is a storage key: two tables sharing one would
// silently apply one screen's widths to another, and a typo would look exactly like "this table
// remembers nothing". Neither is a compile error against `string`. Declaring the set makes the
// first impossible to spell and the second impossible to write, and it gives `tableIds.test.ts`
// something to check the call sites against — they are `.tsx`, which Vitest cannot import, so the
// check has to read them as text (the shape `repo/guards.rs` uses in the backend).
//
// ⚠️ **An id is a name, not a route.** Two tables on one screen need two ids (Reports has three,
// Notification delivery has two), and one component rendered in two places needs two as well when
// its columns differ — `components/EventLog/eventColumns.tsx` builds a different set for `/events`
// than for the node-detail Events tab, so `events.log` and `node.events` are separate.
//
// ⚠️ **Ids are persisted, so renaming one silently drops every operator's widths for that table.**
// There is no migration and there should not be: the cost of a rename is that one table goes back
// to its declared widths, which is the same as never having dragged it. Adding and removing are
// free; renaming is a decision.

/**
 * Every table that can remember its column widths.
 *
 * Keep this grouped by where the table lives, and keep it alphabetical inside a group — the list is
 * read far more often than it is edited.
 */
export const TABLE_IDS = [
  // Node detail
  'node.dnsHealth',
  'node.events',
  'node.flows',
  'node.interfaces',
  'node.neighbors',
  // Alerts
  'alerts.eventRules',
  'alerts.history',
  'alerts.maintenance',
  'alerts.mutes',
  'alerts.thresholds',
  // Nodes / monitoring configuration
  'nodes.classification',
  'nodes.collectionTemplates',
  'nodes.dependencies',
  'nodes.mib',
  // Events
  'events.log',
  'events.sources',
  // Reports
  'reports.definitions',
  'reports.runs',
  'reports.schedules',
  // Troubleshoot
  'troubleshoot.findings',
  'troubleshoot.flowScan',
  'troubleshoot.ruleGap',
  'troubleshoot.scheduled',
  // Settings
  'settings.apiTokens',
  'settings.audit',
  'settings.credentials',
  'settings.forwarding',
  'settings.notificationChannels',
  'settings.pollers',
  'settings.routingRules',
] as const;

/** The name of one table. An unregistered spelling is a compile error at the call site. */
export type TableId = (typeof TABLE_IDS)[number];
