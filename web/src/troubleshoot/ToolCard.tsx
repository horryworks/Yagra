// SPDX-License-Identifier: AGPL-3.0-only
// Tool catalog card (handoff §1). Monogram tile + name + method line, description, a "Surfaces …"
// reveal, and a footer meta row: cost estimate, a 5-pip compute-depth indicator, and a SPLIT Run
// button. The main "Run" opens the launch drawer to set scope/window/depth before running (so the
// scope is never silently "all"); the ▾ menu holds explicit shortcuts — "Configure & run…" and a
// clearly-labelled "Run on all nodes" quick path. Past results show in the Analysis runs panel.

import { useEffect, useRef, useState } from 'react';
import { useTranslation } from 'react-i18next';
import { Button } from '../components/ui/Button';
import { METHODS, type Tool, reportPathFor } from './data';
import { useTroubleshootStore } from './store';
import { defaultAnalysisInput } from './scope';

function DepthPips({ depth }: { depth: number }) {
  const { t } = useTranslation('troubleshoot');
  const label = t('toolCard.depthTitle', { depth });
  return (
    <span className="ts-depth" title={label} aria-label={label}>
      {[1, 2, 3, 4, 5].map((i) => (
        <span key={i} className={i <= depth ? 'ts-depth-pip on' : 'ts-depth-pip'} />
      ))}
    </span>
  );
}

export function ToolCard({ tool }: { tool: Tool }) {
  const { t } = useTranslation('troubleshoot');
  const openDrawer = useTroubleshootStore((s) => s.openDrawer);
  const createJob = useTroubleshootStore((s) => s.createJob);
  const showToast = useTroubleshootStore((s) => s.showToast);
  const method = METHODS[tool.method];

  const [menuOpen, setMenuOpen] = useState(false);
  const menuRef = useRef<HTMLDivElement>(null);

  useEffect(() => {
    if (!menuOpen) return;
    const onDown = (e: MouseEvent) => {
      if (menuRef.current && !menuRef.current.contains(e.target as Node)) setMenuOpen(false);
    };
    const onKey = (e: KeyboardEvent) => {
      if (e.key === 'Escape') setMenuOpen(false);
    };
    document.addEventListener('mousedown', onDown);
    document.addEventListener('keydown', onKey);
    return () => {
      document.removeEventListener('mousedown', onDown);
      document.removeEventListener('keydown', onKey);
    };
  }, [menuOpen]);

  const configure = () => {
    setMenuOpen(false);
    openDrawer(tool.id);
  };

  const quickRunAll = async () => {
    setMenuOpen(false);
    try {
      const job = await createJob(defaultAnalysisInput(tool.id, t));
      showToast(
        t('toolCard.toast.startedAll', { name: t(tool.name) }),
        `${reportPathFor(tool.id)}?job=${job.id}`,
      );
    } catch {
      showToast(t('toast.startFailed'));
    }
  };

  return (
    <article className="ts-tool" onClick={configure}>
      <div className="ts-tool-top">
        <div className="ts-tool-mono">{tool.mono}</div>
        <div className="ts-tool-titles">
          <span className="ts-tool-name">{t(tool.name)}</span>
          <span className="ts-tool-method">
            <span className="ts-tool-method-dot" style={{ background: method.color }} />
            {t(method.label)}
          </span>
        </div>
      </div>
      <p className="ts-tool-desc">{t(tool.desc)}</p>
      <div className="ts-tool-reveal">
        <b>{t('toolCard.surfaces')}</b> {t(tool.reveal)}
      </div>
      <div className="ts-tool-meta">
        <span className="ts-tool-cost">
          <span className="ts-tool-cost-glyph" aria-hidden>
            ◷
          </span>
          {t(tool.est)}
        </span>
        <DepthPips depth={tool.depth} />
        <div className="ts-tool-actions">
          <div className="ts-run-split" ref={menuRef}>
            <Button
              variant="primary"
              className="ts-run-main"
              onClick={(e) => {
                e.stopPropagation();
                configure();
              }}
            >
              {t('actions.run')}
            </Button>
            <Button
              variant="primary"
              className="ts-run-caret"
              aria-haspopup="menu"
              aria-expanded={menuOpen}
              aria-label={t('toolCard.moreOptions')}
              onClick={(e) => {
                e.stopPropagation();
                setMenuOpen((o) => !o);
              }}
            >
              ▾
            </Button>
            {menuOpen && (
              <div className="ts-run-menu" role="menu">
                <button
                  type="button"
                  role="menuitem"
                  className="ts-run-menu-item"
                  onClick={(e) => {
                    e.stopPropagation();
                    configure();
                  }}
                >
                  {t('toolCard.configureRun')}
                </button>
                <button
                  type="button"
                  role="menuitem"
                  className="ts-run-menu-item"
                  onClick={(e) => {
                    e.stopPropagation();
                    void quickRunAll();
                  }}
                >
                  {t('toolCard.runAll')}
                </button>
              </div>
            )}
          </div>
        </div>
      </div>
    </article>
  );
}
