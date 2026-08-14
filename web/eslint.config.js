// ESLint flat config (ESLint v10). The project had no flat `eslint.config.js`, so `npm run lint`
// previously failed to start; this restores linting for the React + TypeScript WebUI.
//
// Scope is intentionally pragmatic: JS + typescript-eslint recommended (no type-aware rules, so
// no tsconfig project wiring needed) plus the React Hooks rules — the bugs that actually bite a
// hooks-based SPA. Tighten further over time.
//
// NB `eslint-plugin-react-hooks` v7 (required for ESLint 10) folded the whole **React Compiler**
// rule pack into its `recommended` preset — ~25 extra rules (`set-state-in-effect`,
// `incompatible-library`, `immutability`, …) that are compiler-optimization advice, not the
// correctness rules this config was built around. Spreading `recommended` here would turn a clean
// lint into 37 errors overnight. So the two original rules are enabled **by name** instead, holding
// the established baseline; adopting the compiler pack is a separate, deliberate decision.
import js from '@eslint/js';
import globals from 'globals';
import tseslint from 'typescript-eslint';
import reactHooks from 'eslint-plugin-react-hooks';
import reactRefresh from 'eslint-plugin-react-refresh';

export default tseslint.config(
  { ignores: ['dist', 'node_modules', 'coverage'] },
  // Application source — browser runtime.
  {
    files: ['src/**/*.{ts,tsx}'],
    extends: [js.configs.recommended, ...tseslint.configs.recommended],
    languageOptions: {
      ecmaVersion: 2022,
      sourceType: 'module',
      globals: globals.browser,
    },
    plugins: {
      'react-hooks': reactHooks,
      'react-refresh': reactRefresh,
    },
    rules: {
      'react-hooks/rules-of-hooks': 'error',
      'react-hooks/exhaustive-deps': 'warn',
      'react-refresh/only-export-components': ['warn', { allowConstantExport: true }],
    },
  },
  // Build/tooling config files — Node runtime.
  {
    files: ['*.{ts,js}'],
    extends: [js.configs.recommended, ...tseslint.configs.recommended],
    languageOptions: {
      ecmaVersion: 2022,
      sourceType: 'module',
      globals: globals.node,
    },
  },
  // The Playwright suite (ADR-052). Node runtime, plus the browser globals the snippets passed to
  // `page.evaluate()` run against — those bodies are TypeScript here but execute in the page.
  // Without this block `tests/` was simply not linted, which is not the same as being clean.
  {
    files: ['tests/**/*.ts'],
    extends: [js.configs.recommended, ...tseslint.configs.recommended],
    languageOptions: {
      ecmaVersion: 2022,
      sourceType: 'module',
      globals: { ...globals.node, ...globals.browser },
    },
  },
);
