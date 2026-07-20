// SPDX-License-Identifier: AGPL-3.0-only
// Bottom-center transient toast for the Troubleshoot section (ui-conventions: transient results
// are toasts, not modals). Driven by the store; auto-dismisses ~4.2s after each showToast. An
// optional "View →" link navigates to a result route. Kept mounted by each troubleshoot page so
// it survives a re-render; the store outlives navigation between section pages.

import { useEffect } from 'react';
import { useNavigate } from 'react-router-dom';
import { useTranslation } from 'react-i18next';
import { useTroubleshootStore } from './store';

export function TroubleshootToast() {
  const { t } = useTranslation('troubleshoot');
  const toast = useTroubleshootStore((s) => s.toast);
  const dismiss = useTroubleshootStore((s) => s.dismissToast);
  const navigate = useNavigate();

  useEffect(() => {
    if (!toast) return;
    const id = setTimeout(dismiss, 4200);
    return () => clearTimeout(id);
  }, [toast, dismiss]);

  return (
    <div className={toast ? 'ts-toast show' : 'ts-toast'} role="status" aria-live="polite">
      {toast && (
        <>
          <span>{toast.msg}</span>
          {toast.linkTo && (
            <button
              className="ts-linkbtn ts-toast-link"
              onClick={() => {
                navigate(toast.linkTo!);
                dismiss();
              }}
            >
              {t('actions.view')} →
            </button>
          )}
        </>
      )}
    </div>
  );
}
