// SPDX-License-Identifier: AGPL-3.0-only
// The tags an Admin registered on Nodes > Tags, offered by the tag fields as suggestions (ADR-263
// Inc.2). Read only by someone who may write tags (ManageConfig, the server's own gate), so a
// Viewer never asks for a list it would be refused.
//
// A suggestion, not a rule: a field never refuses a tag because it is not registered. A failed read
// therefore costs only the suggestions, and the field works as it did before Inc.2.

import { api } from '../services/api';
import { useCan } from '../store';
import { useLoad } from './useLoad';

export function useRegisteredTags(): readonly string[] {
  const canWrite = useCan('manage_config');
  const { data } = useLoad(() => api.listRegisteredTags().then((r) => r.tags), [], {
    initial: [] as string[],
    enabled: canWrite,
  });
  return data;
}
