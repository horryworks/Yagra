// SPDX-License-Identifier: AGPL-3.0-only
// How a notification channel's kind is written for a human.
//
// WHY THIS EXISTS. `CHANNEL_KINDS` in `types/api.ts` carried a doc comment saying "the Routing
// screen's kind filter iterates it — a union the compiler knows and nothing can enumerate at
// runtime is exactly what `extensibility.md` §4 is about". Nothing iterated it. The array was
// referenced by no other file at all, while `RoutingPage.tsx` spelled the four kinds out as
// `<option>` literals and `routingFilters.ts` labelled the same column with the raw token. So the
// list existed in two places, the array that was supposed to be the source was dead, and the two
// places disagreed on screen: the add-channel dialog said "PagerDuty", the filter under the same
// column said "pagerduty".
//
// `Record<ChannelKind, string>` rather than a second array, so a fifth kind is a compile error
// here instead of a missing option nobody notices.
//
// ⚠️ **Not translated, deliberately.** Three of the four are product names (PagerDuty, Jira
// Service Management) which are the same string in every locale, and `webhook` is a protocol.
// Routing these through `t()` would put four keys in both locales whose JA value must equal the
// EN one — a parity gate over strings that can never differ. If a kind ever gains a name that
// genuinely translates, that kind gets a `t()` key and this map is where the exception goes.

import { CHANNEL_KINDS, type ChannelKind } from '../types/api';

const LABELS: Record<ChannelKind, string> = {
  webhook: 'Webhook',
  email: 'Email',
  pagerduty: 'PagerDuty',
  jsm: 'Jira Service Management',
};

/** Display name for a channel kind.
 *
 *  Takes a `string`, not a `ChannelKind`: the value arriving from the API is typed but the filter
 *  builds its options from whatever kinds the rows actually carry, and a core that has gained a
 *  kind this build has never heard of must render as its token rather than as blank. */
export function channelKindLabel(kind: string): string {
  return LABELS[kind as ChannelKind] ?? kind;
}

/** The kinds an operator may create, with their labels — the add/edit dialog's option list. */
export function channelKindOptions(): { value: ChannelKind; label: string }[] {
  return CHANNEL_KINDS.map((k) => ({ value: k, label: LABELS[k] }));
}
