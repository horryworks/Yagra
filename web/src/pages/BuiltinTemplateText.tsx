// SPDX-License-Identifier: AGPL-3.0-only
// Yagra's built-in notification text, drawn read-only (ADR-197 decision 3, Inc.2 decision 10):
// variables as the editor's tags, a part sent only under a condition underlined and numbered, its
// condition said once below, and a whole kept-or-dropped line with its condition at the line's end.
// Also the JSON a JSON channel's preview and built-in body are drawn with (Inc.2 decision 9).
// What to draw is decided in `templateDisplay.ts` and `templateForm.ts`; this file only maps it to
// elements.

import { useTranslation } from 'react-i18next';
import {
  circled,
  describeNote,
  layoutForDisplay,
  readForDisplay,
  type ConditionWords,
  type Shown,
} from './templateDisplay';
import { prettyJson } from './templateForm';

function useConditionWords(): ConditionWords {
  const { t } = useTranslation('alertsConfig');
  return {
    labelOf: (n) => t(`routing.template.vars.${n}.label`, { defaultValue: n }),
    numberOf: circled,
    present: (name) => t('routing.template.builtinView.present', { name }),
    differs: (name, other) => t('routing.template.builtinView.differs', { name, other }),
    and: t('routing.template.builtinView.and'),
    when: (cond) => t('routing.template.builtinView.when', { cond }),
    otherwiseWhen: (prev, cond) => t('routing.template.builtinView.otherwiseWhen', { prev, cond }),
    otherwise: (prev) => t('routing.template.builtinView.otherwise', { prev }),
    inside: (text, parent) => t('routing.template.builtinView.inside', { text, parent }),
  };
}

function Pieces({ shown, words }: { shown: Shown[]; words: ConditionWords }) {
  return (
    <>
      {shown.map((p, i) => {
        switch (p.kind) {
          case 'text':
            return <span key={i}>{p.text}</span>;
          case 'var':
            return (
              <span key={i} className="tpl-chip is-static">
                {words.labelOf(p.name)}
              </span>
            );
          case 'raw':
            return (
              <code key={i} className="tpl-raw">
                {p.text}
              </code>
            );
          case 'part':
            return (
              <span key={i}>
                <span className="tpl-cond-num" aria-hidden="true">
                  {words.numberOf(p.n)}
                </span>
                <span className="tpl-cond-part">
                  <Pieces shown={p.pieces} words={words} />
                </span>
              </span>
            );
          case 'line': {
            // The line's own newline goes after the note, so the note sits at the end of the line.
            const last = p.pieces[p.pieces.length - 1];
            const ends = last?.kind === 'text' && last.text.endsWith('\n');
            const pieces: Shown[] = ends
              ? [...p.pieces.slice(0, -1), { kind: 'text', text: last.text.slice(0, -1) }]
              : p.pieces;
            return (
              <span key={i}>
                <Pieces shown={pieces} words={words} />
                <span className="tpl-line-when">{describeNote(p.note, words)}</span>
                {ends && '\n'}
              </span>
            );
          }
        }
      })}
    </>
  );
}

/** One built-in field, read-only, with its numbered conditions listed below it. */
export function BuiltinTemplateText({
  id,
  source,
  label,
  multiline,
  lead,
}: {
  id: string;
  source: string;
  label: string;
  multiline: boolean;
  /** A tag drawn on the first line in place of text the field repeats from elsewhere. */
  lead?: string;
}) {
  const { t } = useTranslation('alertsConfig');
  const words = useConditionWords();
  const { shown, legend } = layoutForDisplay(readForDisplay(source));
  return (
    <>
      <div
        id={id}
        className={multiline ? 'tpl-field tpl-builtin is-multi is-mono' : 'tpl-field tpl-builtin'}
        role="group"
        aria-label={label}
        data-readonly={t('routing.template.builtinView.readOnly')}
      >
        {lead && <span className="tpl-chip is-static is-lead">{lead}</span>}
        <Pieces shown={shown} words={words} />
      </div>
      {legend.length > 0 && (
        <ol className="tpl-legend" aria-label={t('routing.template.builtinView.legend', { field: label })}>
          {legend.map(({ n, note }) => (
            <li key={n}>
              <span className="tpl-cond-num">{circled(n)}</span>
              <span>{describeNote(note, words)}</span>
            </li>
          ))}
        </ol>
      )}
    </>
  );
}

/** A JSON document laid out one value per line and coloured; plain text when it is not JSON. */
export function JsonText({ text }: { text: string }) {
  const tokens = prettyJson(text);
  if (!tokens) return <>{text}</>;
  return (
    <>
      {tokens.map((tok, i) =>
        tok.kind === 'space' || tok.kind === 'punct' ? (
          tok.text
        ) : (
          <span key={i} className={`tpl-json-${tok.kind}`}>
            {tok.text}
          </span>
        ),
      )}
    </>
  );
}
