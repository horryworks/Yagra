// Tool catalog card (handoff §1). Monogram tile + name + method line, description, a "Surfaces …"
// reveal, and a footer meta row: cost estimate, a 5-pip compute-depth indicator, and the primary
// Run button. Clicking the card (or Run) opens the launch drawer. Past results for a tool show in
// the Analysis runs panel, so the card itself carries no per-tool "latest" link.

import { Button } from '../components/ui/Button';
import { METHODS, type Tool } from './data';
import { useTroubleshootStore } from './store';

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
  const method = METHODS[tool.method];

  return (
    <article className="ts-tool" onClick={() => openDrawer(tool.id)}>
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
          <Button
            variant="primary"
            className="btn-sm"
            onClick={(e) => {
              e.stopPropagation();
              openDrawer(tool.id);
            }}
          >
            Run
          </Button>
        </div>
      </div>
    </article>
  );
}
