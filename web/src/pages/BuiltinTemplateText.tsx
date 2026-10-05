// SPDX-License-Identifier: AGPL-3.0-only
// Yagra's built-in notification text, drawn read-only (ADR-197 decision 3): variables as the
// editor's tags, and each part sent only under a condition inside a dashed box that says when.
// What to draw is decided in `templateDisplay.ts`; this file only maps pieces to elements.

import { useTranslation } from 'react-i18next';
import { describeBranch, readForDisplay, type ConditionWords, type DisplayPiece } from './templateDisplay';

function Pieces({ pieces, words }: { pieces: DisplayPiece[]; words: ConditionWords }) {
  return (
    <>
      {pieces.map((p, i) => {
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
          case 'cond':
            return (
              <span key={i} className="tpl-cond-group">
                {p.branches.map((b, j) => (
                  <span key={j} className="tpl-cond">
                    <span className="tpl-cond-cap">{describeBranch(b, words)}</span>
                    <Pieces pieces={b.pieces} words={words} />
                  </span>
                ))}
              </span>
            );
        }
      })}
    </>
  );
}

/** One built-in field, read-only. */
export function BuiltinTemplateText({
  id,
  source,
  label,
  multiline,
}: {
  id: string;
  source: string;
  label: string;
  multiline: boolean;
}) {
  const { t } = useTranslation('alertsConfig');
  const words: ConditionWords = {
    labelOf: (n) => t(`routing.template.vars.${n}.label`, { defaultValue: n }),
    present: (name) => t('routing.template.builtinView.present', { name }),
    differs: (name, other) => t('routing.template.builtinView.differs', { name, other }),
    and: t('routing.template.builtinView.and'),
    when: (cond) => t('routing.template.builtinView.when', { cond }),
    otherwiseWhen: (cond) => t('routing.template.builtinView.otherwiseWhen', { cond }),
    otherwise: t('routing.template.builtinView.otherwise'),
  };
  return (
    <div
      id={id}
      className={multiline ? 'tpl-field tpl-builtin is-multi' : 'tpl-field tpl-builtin'}
      role="group"
      aria-label={label}
      data-readonly={t('routing.template.builtinView.readOnly')}
    >
      <Pieces pieces={readForDisplay(source)} words={words} />
    </div>
  );
}
