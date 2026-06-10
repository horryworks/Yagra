// In-page sub-tabs (§4: detail screens use page-internal sub-tabs, e.g. node detail
// Overview/Interfaces). Controlled: the parent owns the active key. Active = 朱 underline,
// consistent with the top-bar tab treatment.

import './Tabs.css';

export interface TabDef {
  key: string;
  label: string;
}

interface Props {
  tabs: TabDef[];
  active: string;
  onChange: (key: string) => void;
}

export function Tabs({ tabs, active, onChange }: Props) {
  return (
    <div className="tabs" role="tablist">
      {tabs.map((t) => (
        <button
          key={t.key}
          role="tab"
          aria-selected={t.key === active}
          className={t.key === active ? 'tab active' : 'tab'}
          onClick={() => onChange(t.key)}
        >
          {t.label}
        </button>
      ))}
    </div>
  );
}
