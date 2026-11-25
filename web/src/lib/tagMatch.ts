// SPDX-License-Identifier: AGPL-3.0-only
// The words for how a setting's tags combine (ADR-267), shared by every place that narrows by tags:
// the routing rule and grouping exception target (`AppliesTo`) and the All nodes tag filter. One
// place, so the switch and the word between two tags read the same on every screen.

import type { TagMatch } from '../types/api';

/** The label of each side of the has any / has all switch. Spelled out, so no key is built at
 *  runtime. */
export const TAG_MATCH_LABEL: Record<TagMatch, 'common:tagMatch.any' | 'common:tagMatch.all'> = {
  any: 'common:tagMatch.any',
  all: 'common:tagMatch.all',
};

/** The word said between two tags: "or" when any one is enough, "and" when every one is needed
 *  (ADR-267 decision 6). */
export const TAG_WORD: Record<TagMatch, 'common:tagMatch.or' | 'common:tagMatch.and'> = {
  any: 'common:tagMatch.or',
  all: 'common:tagMatch.and',
};
