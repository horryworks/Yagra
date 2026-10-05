// SPDX-License-Identifier: AGPL-3.0-only
// One shell command, shown whole and copied with one press (ADR-200).
//
// A command an operator has to retype is a command they mistype, and the ones on the Upgrade page
// run against a production composition. The text wraps rather than truncating: a command cut off
// with an ellipsis is a command nobody can check before pasting it.

import { useTranslation } from 'react-i18next';
import { useCopy } from '../../lib/useCopy';
import { IconButton } from './IconButton';
import { CopyIcon } from './icons';
import './CopyCommand.css';

export function CopyCommand({ command }: { command: string }) {
  const { t } = useTranslation();
  const { copied, copy } = useCopy();
  return (
    <span className="copy-cmd">
      <code className="copy-cmd-text">{command}</code>
      <IconButton
        className="copy-cmd-btn"
        title={copied !== null ? t('copy.copied') : t('copy.copyCommand')}
        onClick={() => copy(command)}
      >
        <CopyIcon />
      </IconButton>
    </span>
  );
}
