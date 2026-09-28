// SPDX-License-Identifier: AGPL-3.0-only
// Copying text to the clipboard and flashing a confirmation, one way (ADR-184).
//
// Six components carried this as a local closure — write, set a flag, clear it on a timer — and
// none of them cleared the timer on unmount, so closing a dialog within the flash set state on an
// unmounted component. `mark` names *which* control was copied, for a dialog with two copy buttons
// (a token and a URL); with one button it defaults to the text itself and `copied !== null` is the
// whole question.

import { useCallback, useEffect, useRef, useState } from 'react';

/** How long the "Copied" confirmation stays up, unless a caller has a reason to differ. */
export const COPY_FLASH_MS = 1200;

export function useCopy(flashMs: number = COPY_FLASH_MS): {
  copied: string | null;
  copy: (text: string, mark?: string) => void;
} {
  const [copied, setCopied] = useState<string | null>(null);
  const timer = useRef<ReturnType<typeof setTimeout> | null>(null);
  useEffect(
    () => () => {
      if (timer.current) clearTimeout(timer.current);
    },
    [],
  );
  const copy = useCallback(
    (text: string, mark: string = text) => {
      void navigator.clipboard?.writeText(text);
      setCopied(mark);
      if (timer.current) clearTimeout(timer.current);
      timer.current = setTimeout(() => setCopied(null), flashMs);
    },
    [flashMs],
  );
  return { copied, copy };
}
