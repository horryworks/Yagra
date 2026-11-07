// SPDX-License-Identifier: AGPL-3.0-only
// Types for `ui-rules.mjs`, so `src/uiBudget.test.ts` can import it under `strict`.

export type Metric = 'spacing' | 'color' | 'fontSize' | 'inlineSpacing' | 'legacy';

export interface Finding {
  line: number;
  prop: string;
  value: string;
  raw: string;
}

export interface Measured {
  spacing: Finding[];
  color: Finding[];
  fontSize: Finding[];
  inlineSpacing: Finding[];
  legacy: Finding[];
  /** Lines of a page-local stylesheet; `null` for every other file. */
  lines: number | null;
  allowProblems: { line: number; problem: string }[];
  /** Declarations, style keys and class names looked at - what a floor counts. */
  inspected: number;
}

export interface Budget {
  _doc?: string;
  spacing?: Record<string, number>;
  color?: Record<string, number>;
  fontSize?: Record<string, number>;
  inlineSpacing?: Record<string, number>;
  legacy?: Record<string, number>;
  pageCss?: Record<string, { lines: number; why: string }>;
}

export interface Problem {
  kind: 'over' | 'lower' | 'unlisted' | 'noWhy' | 'allow';
  metric: Metric | 'lines' | 'allow';
  count?: number;
  ceiling?: number;
  items?: Finding[];
  line?: number;
  problem?: string;
}

export const WEB: string;
export const SRC: string;
export const BUDGET_PATH: string;
export const SPACE_STEPS: [number, string][];
export const METRICS: Metric[];
export const SLACK: { count: number; lines: number };
export const LEGACY_CLASSES: string[];

export function isPageCss(rel: string): boolean;
export function measureCss(rel: string, text: string): Measured;
export function measureTsx(rel: string, text: string): Measured;
export function measure(rel: string, text: string): Measured | null;
export function suggest(v: Pick<Finding, 'prop' | 'raw'>): string;
export function ceilingOf(budget: Budget, metric: Metric, rel: string): number;
export function compare(rel: string, result: Measured, budget: Budget): Problem[];
export function describe(rel: string, p: Problem): string;
export function isBlocking(p: Problem): boolean;
export function readBudget(path?: string): Budget;
export function listFiles(root?: string): [string, string][];
export function writeBudget(budget: Budget, path?: string): void;
