// Placeholder for IA entries whose backend isn't built yet (.claude/docs/design-system.md §5 lists these
// as 🔶 spec-only / ⏸ deferred). The nav entry is intentionally present so the information
// architecture stays whole; this screen states plainly that the backend is pending so it's
// never mistaken for a broken page. Resolves its own title from the nav by current path.

import { useLocation } from 'react-router-dom';
import { NAV } from '../../nav';
import { PageHeader } from './PageHeader';
import { Card } from './Card';
import './ComingSoon.css';

function labelForPath(pathname: string): { section: string; label: string } {
  for (const s of NAV) {
    for (const item of s.items) {
      if (item.path === pathname) return { section: s.label, label: item.label };
    }
  }
  return { section: '', label: 'This screen' };
}

export function ComingSoon() {
  const { pathname } = useLocation();
  const { section, label } = labelForPath(pathname);
  const trail = section ? [{ label: section }, { label }] : [{ label }];

  return (
    <div>
      <PageHeader title={label} trail={trail} />
      <Card>
        <div className="comingsoon">
          <div className="comingsoon-badge">Coming soon</div>
          <p className="comingsoon-text">
            {label} is part of the planned information architecture. Its screen will appear here
            once the backing API lands — the navigation entry is kept so the structure stays
            stable. Check back in a future release.
          </p>
        </div>
      </Card>
    </div>
  );
}
