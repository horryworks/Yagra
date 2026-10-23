// SPDX-License-Identifier: AGPL-3.0-only
// Catches a lazily loaded screen whose chunk could not be fetched (ADR-212).
//
// Without it the rejected import escaped React entirely and unmounted the whole app: a blank page,
// with the shell gone too. It sits around `LazyGroup`'s Suspense, so the top bar and the sidebar
// stay and the operator can still go elsewhere.
//
// It catches only what `isChunkLoadError` recognises. Any other render error is thrown again from
// `render`, which hands it to whatever caught it before this existed.

import { Component, type ReactNode } from 'react';
import { useTranslation } from 'react-i18next';
import { Button } from '../ui/Button';
import { EmptyState } from '../ui/EmptyState';
import { isChunkLoadError } from '../../lib/chunkLoad';

interface Props {
  /** Changes when the operator moves to another screen, which clears a caught failure: another
   *  group's chunk may well load. (`React.lazy` remembers a failure, so the same group does not.) */
  resetKey: string;
  children: ReactNode;
}

interface State {
  error: unknown;
  resetKey: string;
}

export class ChunkErrorBoundary extends Component<Props, State> {
  state: State = { error: null, resetKey: this.props.resetKey };

  static getDerivedStateFromError(error: unknown): Partial<State> {
    return { error };
  }

  static getDerivedStateFromProps(props: Props, state: State): Partial<State> | null {
    return props.resetKey === state.resetKey ? null : { error: null, resetKey: props.resetKey };
  }

  render() {
    const { error } = this.state;
    if (error === null) return this.props.children;
    if (!isChunkLoadError(error)) throw error;
    return <ChunkLoadFailed />;
  }
}

function ChunkLoadFailed() {
  const { t } = useTranslation();
  return (
    <EmptyState
      text={t('chunkError.text')}
      action={
        <Button type="button" variant="primary" onClick={() => window.location.reload()}>
          {t('chunkError.reload')}
        </Button>
      }
    />
  );
}
