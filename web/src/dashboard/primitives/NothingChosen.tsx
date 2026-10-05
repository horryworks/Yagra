// SPDX-License-Identifier: AGPL-3.0-only
// What a widget whose subject is chosen in its ⚙ panel shows before anything is chosen (ADR-200).
//
// While the board is being customized the frame hands the body `openSettings`, and the choice is a
// button on the card itself. Outside Customize the panel does not exist (ADR-072), so the card says
// what is missing and where it is set up, in two short sentences.

import { useTranslation } from 'react-i18next';
import { Button } from '../../components/ui/Button';
import { EmptyState } from '../../components/ui/EmptyState';

export function NothingChosen({
  text,
  choose,
  openSettings,
}: {
  /** The state, one sentence: "No interfaces chosen." */
  text: string;
  /** The button's label: "Choose interfaces". */
  choose: string;
  /** From `WidgetProps` — present only while customizing. */
  openSettings?: () => void;
}) {
  const { t } = useTranslation('dashboard');
  if (openSettings) {
    return (
      <EmptyState
        text={text}
        action={
          <Button type="button" onClick={openSettings}>
            {choose}
          </Button>
        }
      />
    );
  }
  return (
    <p className="muted">
      {text} {t('widgetFrame.setUpInCustomize')}
    </p>
  );
}
