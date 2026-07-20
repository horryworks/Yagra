// SPDX-License-Identifier: AGPL-3.0-only
// Button (§4): outline by default; only the primary action gets a subtle fill. Variants:
// `outline` (default), `primary` (filled 藍), `danger` (destructive), `ghost` (toolbar).

import type { ButtonHTMLAttributes } from 'react';
import './Button.css';

type Variant = 'outline' | 'primary' | 'danger' | 'ghost';

interface Props extends ButtonHTMLAttributes<HTMLButtonElement> {
  variant?: Variant;
}

export function Button({ variant = 'outline', className, ...rest }: Props) {
  const cls = ['btn', `btn-${variant}`, className].filter(Boolean).join(' ');
  return <button className={cls} {...rest} />;
}
