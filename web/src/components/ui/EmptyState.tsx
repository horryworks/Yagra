// SPDX-License-Identifier: AGPL-3.0-only
// What an empty list says (ADR-200): one sentence and the next step, as a button or a link.
//
// The sentence states the state ("No API tokens yet."); the action is what the old second sentence
// used to describe ("Create one to give…"). Pass it to `DataTable`'s `empty`.

import type { ReactNode } from 'react';
import './EmptyState.css';

export function EmptyState({ text, action }: { text: ReactNode; action?: ReactNode }) {
  return (
    <div className="empty-state">
      <p className="empty-state-text">{text}</p>
      {action && <div className="empty-state-action">{action}</div>}
    </div>
  );
}
