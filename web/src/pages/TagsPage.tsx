// SPDX-License-Identifier: AGPL-3.0-only
// Nodes > Tags (ADR-263). Every tag the deployment names, where each is named, and a rename or a
// delete that reaches all of those places at once: the nodes, the folders, the refusals, the
// notification routing rules and the alert-grouping exceptions.
//
// A rename onto a tag that already exists merges the two; the dialog says so before the save. A
// delete a routing rule or an exception depends on is refused by the server (409), and its message
// names the rules, so the error line is the explanation.
//
// Since ADR-263 Inc.2 a tag can also be added by name before anything carries it ("+ Add tag"):
// it is registered, listed with every count at zero, and offered by the tag fields.
//
// The judgement (the new name's rules, which tag a rename merges into) is `tagCatalog.ts`, where
// a test can reach it.

import { useMemo, useState } from 'react';
import { Trans, useTranslation } from 'react-i18next';
import { api } from '../services/api';
import type { NodeTag, NodeTags } from '../types/api';
import { Page } from '../components/ui/Page';
import { PageHeader } from '../components/ui/PageHeader';
import { DataTable, type Column } from '../components/ui/DataTable';
import { LoadGate } from '../components/ui/LoadGate';
import { Modal } from '../components/ui/Modal';
import { ConfirmDeleteModal } from '../components/ui/ConfirmDeleteModal';
import { Field, TextInput } from '../components/ui/Field';
import { Fields } from '../components/ui/FormLayout';
import { FormError, FormFooter } from '../components/ui/FormFooter';
import { OverflowMenu } from '../components/ui/OverflowMenu';
import { TableToolbar, TableSpacer } from '../components/ui/TableToolbar';
import { Button } from '../components/ui/Button';
import { EditIcon, TrashIcon } from '../components/ui/icons';
import { useLoad } from '../lib/useLoad';
import { useSubmit } from '../lib/useSubmit';
import { done } from '../lib/submitState';
import { isImeComposing } from '../lib/ime';
import { rowActionsWidth } from '../lib/rowActions';
import { useCan } from '../store';
import { placesNamed, planAdd, planRename } from './tagCatalog';

export function TagsPage() {
  const { t } = useTranslation('nodes');
  const canSystem = useCan('manage_system');
  const [renaming, setRenaming] = useState<NodeTag | null>(null);
  const [deleting, setDeleting] = useState<NodeTag | null>(null);
  const [adding, setAdding] = useState(false);

  const list = useLoad(() => api.listNodeTags(), [], { initial: null as NodeTags | null });
  const { data, loading, reload } = list;
  const rows = useMemo(() => data?.tags ?? [], [data]);

  const columns = useMemo<Column<NodeTag>[]>(() => {
    const count = (key: keyof NodeTag, header: string, width = '110px'): Column<NodeTag> => ({
      key,
      header,
      width,
      align: 'right',
      render: (r) => (r[key] === 0 ? <span className="muted">0</span> : r[key]),
    });
    return [
      {
        key: 'tag',
        header: t('tags.cols.tag'),
        width: '2fr',
        render: (r) => <span title={r.tag}>{r.tag}</span>,
      },
      count('nodes', t('tags.cols.nodes')),
      count('own', t('tags.cols.own')),
      count('folders', t('tags.cols.folders')),
      count('rules', t('tags.cols.rules')),
      count('exceptions', t('tags.cols.exceptions'), '170px'),
      {
        key: 'registered',
        header: t('tags.cols.registered'),
        width: '110px',
        align: 'right',
        render: (r) => (r.registered ? '✓' : <span className="muted">—</span>),
      },
      {
        key: 'actions',
        header: t('tags.cols.actions'),
        // Rename and delete.
        width: rowActionsWidth(2),
        align: 'right',
        render: (r) =>
          canSystem ? (
            <span className="ytable-actions">
              <OverflowMenu
                actions={[
                  { label: t('tags.rename'), icon: <EditIcon />, onClick: () => setRenaming(r) },
                  {
                    label: t('tags.delete'),
                    icon: <TrashIcon />,
                    danger: true,
                    onClick: () => setDeleting(r),
                  },
                ]}
              />
            </span>
          ) : null,
      },
    ];
  }, [t, canSystem]);

  return (
    <Page layout="list">
      <PageHeader
        title={t('nav:nodes.tags')}
        trail={[{ label: t('nav:sections.nodes') }, { label: t('nav:nodes.tags') }]}
      />
      <LoadGate load={list} permission="manage_system">
        {canSystem && (
          <TableToolbar>
            <TableSpacer />
            <Button onClick={() => setAdding(true)}>{t('tags.add')}</Button>
          </TableToolbar>
        )}
        <DataTable
          tableId="nodes.tags"
          rows={rows}
          columns={columns}
          rowKey={(r) => r.tag}
          loading={loading}
          empty={t('tags.empty')}
        />
      </LoadGate>

      {adding && (
        <AddTagModal
          tags={rows}
          onClose={() => setAdding(false)}
          onDone={() => {
            setAdding(false);
            reload();
          }}
        />
      )}
      {renaming && (
        <RenameTagModal
          tag={renaming}
          tags={rows}
          onClose={() => setRenaming(null)}
          onDone={() => {
            setRenaming(null);
            reload();
          }}
        />
      )}
      {deleting && (
        <ConfirmDeleteModal
          title={t('tags.deleteTitle', { tag: deleting.tag })}
          errorFallback={t('tags.err.delete')}
          onConfirm={() => api.deleteNodeTag(deleting.tag)}
          onClose={() => setDeleting(null)}
          onDone={() => {
            setDeleting(null);
            reload();
          }}
        >
          <Trans
            t={t}
            i18nKey="tags.deleteBody"
            values={{ tag: deleting.tag, count: placesNamed(deleting) }}
            components={{ b: <strong /> }}
          />
        </ConfirmDeleteModal>
      )}
    </Page>
  );
}

function RenameTagModal({
  tag,
  tags,
  onClose,
  onDone,
}: {
  tag: NodeTag;
  tags: readonly NodeTag[];
  onClose: () => void;
  onDone: () => void;
}) {
  const { t } = useTranslation('nodes');
  const [name, setName] = useState(tag.tag);
  const form = useSubmit({ errorFallback: t('tags.err.rename'), onDone });
  const plan = planRename(tag.tag, name, tags);
  const touched = name !== tag.tag;

  const submit = () => {
    if (plan.problem !== null) return;
    form.submit(() => api.renameNodeTag(tag.tag, name.trim()).then(() => done()));
  };

  return (
    <Modal
      title={t('tags.renameTitle', { tag: tag.tag })}
      onClose={onClose}
      footer={
        <FormFooter
          form={form}
          onClose={onClose}
          onSubmit={submit}
          submitLabel={plan.mergeInto ? t('tags.merge') : t('tags.rename')}
          canSubmit={plan.problem === null}
        />
      }
    >
      <Fields>
        <Field
          label={t('tags.newName')}
          status={
            plan.mergeInto
              ? t('tags.mergesInto', { tag: plan.mergeInto.tag, count: plan.mergeInto.nodes })
              : undefined
          }
          error={
            !touched || plan.problem === null
              ? null
              : plan.problem === 'same'
                ? t('tags.problem.same')
                : t(`field.tagErr.${plan.problem}`)
          }
        >
          <TextInput
            value={name}
            onChange={(e) => setName(e.target.value)}
            onKeyDown={(e) => {
              // A tag is often Japanese: Enter that commits an IME candidate must not save.
              if (e.key === 'Enter' && !isImeComposing(e)) submit();
            }}
            disabled={form.busy}
            autoFocus
            autoComplete="off"
            spellCheck={false}
          />
        </Field>
        <p className="modal-confirm-text">{t('tags.renameWarn')}</p>
        <FormError form={form} />
      </Fields>
    </Modal>
  );
}

function AddTagModal({
  tags,
  onClose,
  onDone,
}: {
  tags: readonly NodeTag[];
  onClose: () => void;
  onDone: () => void;
}) {
  const { t } = useTranslation('nodes');
  const [name, setName] = useState('');
  const form = useSubmit({ errorFallback: t('tags.err.add'), onDone });
  const plan = planAdd(name, tags);

  const submit = () => {
    if (plan.problem !== null) return;
    form.submit(() => api.registerNodeTag(name.trim()).then(() => done()));
  };

  return (
    <Modal
      title={t('tags.addTitle')}
      onClose={onClose}
      footer={
        <FormFooter
          form={form}
          onClose={onClose}
          onSubmit={submit}
          submitLabel={t('tags.register')}
          canSubmit={plan.problem === null}
        />
      }
    >
      <Fields>
        <Field
          label={t('tags.name')}
          status={plan.listed ? t('tags.alreadyListed') : undefined}
          error={name === '' || plan.problem === null ? null : t(`field.tagErr.${plan.problem}`)}
        >
          <TextInput
            value={name}
            onChange={(e) => setName(e.target.value)}
            onKeyDown={(e) => {
              // A tag is often Japanese: Enter that commits an IME candidate must not save.
              if (e.key === 'Enter' && !isImeComposing(e)) submit();
            }}
            disabled={form.busy}
            autoFocus
            autoComplete="off"
            spellCheck={false}
          />
        </Field>
        <FormError form={form} />
      </Fields>
    </Modal>
  );
}
