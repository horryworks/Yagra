// SPDX-License-Identifier: AGPL-3.0-only
// The dialog Nodes ▸ Missing IP prefixes opens (ADR-170 Inc.4): marking one site's missing subnet
// as intentional. The shape is Subnet overlaps' `AckOverlapModal`, so the two read as one family.

import { useState } from 'react';
import { useTranslation } from 'react-i18next';
import { api } from '../services/api';
import type { PrefixGap, SitePrefixGaps } from '../types/api';
import { done } from '../lib/submitState';
import { useSubmit } from '../lib/useSubmit';
import { Modal } from '../components/ui/Modal';
import { Field, TextInput } from '../components/ui/Field';
import { Fields } from '../components/ui/FormLayout';
import { FormError, FormFooter } from '../components/ui/FormFooter';

/** "This subnet is meant to stay out of this site's IP prefixes", with an optional note. */
export function AckGapModal({
  site,
  siteName,
  gap,
  onClose,
  onDone,
}: {
  site: SitePrefixGaps;
  siteName: string;
  gap: PrefixGap;
  onClose: () => void;
  onDone: () => void;
}) {
  const { t } = useTranslation('monitoring');
  const [note, setNote] = useState('');
  const form = useSubmit({ errorFallback: t('missingPrefixes.ack.err'), onDone });

  return (
    <Modal
      title={t('missingPrefixes.ack.title', { subnet: gap.subnet, site: siteName })}
      onClose={onClose}
      footer={
        <FormFooter
          form={form}
          onClose={onClose}
          onSubmit={() =>
            form.submit(() =>
              api.ackPrefixGap(site.site_id ?? null, gap.subnet, note.trim()).then(() => done()),
            )
          }
          submitLabel={t('missingPrefixes.ack.save')}
        />
      }
    >
      <Fields>
        <Field label={t('missingPrefixes.ack.note')}>
          <TextInput
            value={note}
            placeholder={t('missingPrefixes.ack.notePlaceholder')}
            onChange={(e) => setNote(e.target.value)}
            maxLength={200}
            autoFocus
          />
        </Field>
        <FormError form={form} />
      </Fields>
    </Modal>
  );
}
