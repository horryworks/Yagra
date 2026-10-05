// SPDX-License-Identifier: AGPL-3.0-only
// The step frame (ADR-200 kind d): numbered steps, and the commands they need, behind a disclosure
// that is closed until pressed.
//
// What goes in here is a procedure the operator carries out somewhere else — at a remote site, in
// an identity provider's console — and nothing else. A paragraph about how a feature works is not a
// procedure and does not belong in one: write the steps as imperatives, one per line, and let the
// screen's labels say the rest. One frame per dialog at most.
//
// A native `<details>`: keyboard and screen-reader support come with the element, and a closed
// frame takes no room on the screen it sits in.

import type { ReactNode } from 'react';
import './StepFrame.css';

export function StepFrame({
  summary,
  steps,
  className,
}: {
  summary: string;
  steps: ReactNode[];
  /** Spacing from the caller's layout; the frame itself carries no outer margin. */
  className?: string;
}) {
  return (
    <details className={['step-frame', className].filter(Boolean).join(' ')}>
      <summary>{summary}</summary>
      <ol>
        {steps.map((s, i) => (
          <li key={i}>{s}</li>
        ))}
      </ol>
    </details>
  );
}
