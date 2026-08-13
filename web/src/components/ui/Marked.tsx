// SPDX-License-Identifier: AGPL-3.0-only
// Render a string with the filter's matches marked (ADR-053 Inc.2e).
//
// All the judgement — *which* parts matched, and whether anything should be marked at all — is in
// `lib/matchRanges.ts`, where tests actually run. This file only turns segments into elements.
//
// ⚠️ **Text, never markup.** The message is device-supplied and this repo forbids
// `dangerouslySetInnerHTML` on it in three separate file headers. Segments arrive as data and are
// rendered as children, so a message containing `<script>` is displayed, not executed.

import { markedSegments, matchRanges, type MatchSemantics } from '../../lib/matchRanges';
import type { TextCondition } from '../../lib/filterCondition';
import './Marked.css';

export interface MarkedHighlight {
  cond: TextCondition | null;
  semantics: MatchSemantics;
  widened: boolean;
}

export function Marked({ text, highlight }: { text: string; highlight?: MarkedHighlight }) {
  if (!highlight?.cond) return <>{text}</>;
  const ranges = matchRanges(text, highlight.cond, highlight.semantics, highlight.widened);
  if (ranges.length === 0) return <>{text}</>;
  return (
    <>
      {markedSegments(text, ranges).map((seg, i) =>
        seg.marked ? (
          <mark className="mk" key={i}>
            {seg.text}
          </mark>
        ) : (
          <span key={i}>{seg.text}</span>
        ),
      )}
    </>
  );
}
