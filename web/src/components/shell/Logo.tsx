// Brand mark (§1.1) — seal (朱印) style, unified everywhere: a brand-orange rounded tile
// with an off-white 生成り kamon (二重輪 + 入れ子の山形/矢羽). No outline-only transparent
// logo. Two renderings of the same seal:
//   - 'seal' (default): the orange tile + mark — top bar / home / app icon.
//   - 'mark': just the off-white kamon on transparent — for placing ON a brand-orange
//     surface (the login panel is itself the seal at panel scale).
// NOTE: .claude/docs/design-system.md says reference assets/logo/ and not recreate the logo, but no logo
// assets exist in the repo yet; this is a faithful placeholder. Brand colors are the one
// hardcode exemption (a physical brand surface, fixed in both themes — §1.2).

interface Props {
  size?: number;
  variant?: 'seal' | 'mark';
}

const BRAND = '#e95d08'; // fixed brand orange (--brand-fixed)
const MARK = '#faf7f3'; // off-white 生成り mark (--brand-mark)

export function Logo({ size = 28, variant = 'seal' }: Props) {
  // The kamon is always the off-white mark; only the orange tile is variant-specific.
  const strokeColor = MARK;
  return (
    <svg
      width={size}
      height={size}
      viewBox="0 0 48 48"
      role="img"
      aria-label="Yagra"
      fill="none"
      xmlns="http://www.w3.org/2000/svg"
    >
      {variant === 'seal' && <rect x="1" y="1" width="46" height="46" rx="11" fill={BRAND} />}
      {/* kamon: nested double ring + chevrons / 矢羽 */}
      <circle cx="24" cy="24" r="15.5" stroke={strokeColor} strokeWidth="1.6" />
      <circle cx="24" cy="24" r="11" stroke={strokeColor} strokeWidth="1" opacity="0.85" />
      <path
        d="M15 29 L24 19 L33 29"
        stroke={strokeColor}
        strokeWidth="2.4"
        strokeLinejoin="round"
        strokeLinecap="round"
      />
      <path
        d="M18 32 L24 25.5 L30 32"
        stroke={strokeColor}
        strokeWidth="1.8"
        strokeLinejoin="round"
        strokeLinecap="round"
      />
    </svg>
  );
}
