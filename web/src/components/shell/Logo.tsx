// Brand mark (§1.1: 家紋風ラウンデル — double ring + nested chevron/矢羽 + 朱 dot).
// NOTE: design-system.md says reference assets/logo/ and do not recreate the logo, but no
// logo assets exist in the repo yet. This is a faithful placeholder built from the §1.1
// description; swap it for the real asset once assets/logo/ lands. Brand colors are the one
// hardcode exemption (a physical brand surface, fixed in both themes — §1.2).

interface Props {
  size?: number;
}

const AI = '#1b3a5b'; // 藍
const SHU = '#c7402e'; // 朱
const KINARI = '#f2ede0'; // 生成り

export function Logo({ size = 28 }: Props) {
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
      <circle cx="24" cy="24" r="22.5" stroke={AI} strokeWidth="2" />
      <circle cx="24" cy="24" r="17.5" stroke={AI} strokeWidth="1" />
      {/* nested chevrons / 矢羽 */}
      <path d="M13 30 L24 18 L35 30" stroke={AI} strokeWidth="2.5" strokeLinejoin="round" />
      <path d="M16 34 L24 25 L32 34" stroke={AI} strokeWidth="2" strokeLinejoin="round" />
      {/* 朱の点 */}
      <circle cx="24" cy="13.5" r="2.6" fill={SHU} />
      <circle cx="24" cy="24" r="0" fill={KINARI} />
    </svg>
  );
}
