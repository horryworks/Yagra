// Tool catalog card (handoff §1). Monogram tile + name + method line, description, a "Surfaces …"
// reveal, and a footer meta row: cost estimate, a 5-pip compute-depth indicator, and a SPLIT Run
// button. The main "Run" opens the launch drawer to set scope/window/depth before running (so the
// scope is never silently "all"); the ▾ menu holds explicit shortcuts — "Configure & run…" and a
// clearly-labelled "Run on all nodes" quick path. Past results show in the Analysis runs panel.

import { useEffect, useRef, useState } from 'react';
import { Button } from '../components/ui/Button';
import { METHODS, type Tool } from './data';
import { useTroubleshootStore } from './store';
import { defaultAnalysisInput } from './scope';

function DepthPips({ depth }: { depth: number }) {
  return (
    <span
      className="ts-depth"
      title={`Compute depth ${depth} of 5`}
      aria-label={`Compute depth ${depth} of 5`}
    >
      {[1, 2, 3, 4, 5].map((i) => (
        <span key={i} className={i <= depth ? 'ts-depth-pip on' : 'ts-depth-pip'} />
      ))}
    </span>
  );
}

export function ToolCard({ tool }: { tool: Tool }) {
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
      const job = await createJob(defaultAnalysisInput(tool.id));
      showToast(
        `${tool.name} started on all nodes — running in background.`,
        tool.reportPath ? `${tool.reportPath}?job=${job.id}` : undefined,
      );
    } catch {
      showToast('Could not start the analysis.');
    }
  };

  return (
    <article className="ts-tool" onClick={configure}>
      <div className="ts-tool-top">
        <div className="ts-tool-mono">{tool.mono}</div>
        <div className="ts-tool-titles">
          <span className="ts-tool-name">{tool.name}</span>
          <span className="ts-tool-method">
            <span className="ts-tool-method-dot" style={{ background: method.color }} />
            {method.label}
          </span>
        </div>
      </div>
      <p className="ts-tool-desc">{tool.desc}</p>
      <div className="ts-tool-reveal">
        <b>Surfaces</b> {tool.reveal}
      </div>
      <div className="ts-tool-meta">
        <span className="ts-tool-cost">
          <span className="ts-tool-cost-glyph" aria-hidden>
            ◷
          </span>
          {tool.est}
        </span>
        <DepthPips depth={tool.depth} />
        <div className="ts-tool-actions">
          <div className="ts-run-split" ref={menuRef}>
            <Button
              variant="primary"
              className="btn-sm ts-run-main"
              onClick={(e) => {
                e.stopPropagation();
                configure();
              }}
            >
              Run
            </Button>
            <Button
              variant="primary"
              className="btn-sm ts-run-caret"
              aria-haspopup="menu"
              aria-expanded={menuOpen}
              aria-label="More run options"
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
                  Configure &amp; run…
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
                  Run on all nodes
                </button>
              </div>
            )}
          </div>
        </div>
      </div>
    </article>
  );
}
