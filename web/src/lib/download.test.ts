// SPDX-License-Identifier: AGPL-3.0-only
import { describe, it, expect } from 'vitest';
import { filenameFromDisposition } from './download';

describe('filenameFromDisposition', () => {
  it('reads the quoted filename the API actually sends', () => {
    // Exactly what `api/support.rs` and `api/reports.rs` put on the wire.
    expect(
      filenameFromDisposition('attachment; filename="yagra-support-20260806T101530Z.tar.gz"'),
    ).toBe('yagra-support-20260806T101530Z.tar.gz');
    expect(filenameFromDisposition('attachment; filename="report-abc.pdf"')).toBe('report-abc.pdf');
  });

  it('prefers filename* over filename, and decodes it', () => {
    // RFC 5987. Nothing in Yagra emits this today; being tolerant of it costs one regex and means
    // a future non-ASCII name does not silently download as "download".
    expect(
      filenameFromDisposition(
        "attachment; filename=\"fallback.txt\"; filename*=UTF-8''%E5%A0%B1%E5%91%8A.txt",
      ),
    ).toBe('報告.txt');
  });

  it('falls back to the plain filename when filename* is malformed', () => {
    // Losing the name must never lose the download.
    expect(
      filenameFromDisposition("attachment; filename=\"ok.tar.gz\"; filename*=UTF-8''%E5%A0"),
    ).toBe('ok.tar.gz');
  });

  it('accepts an unquoted filename', () => {
    expect(filenameFromDisposition('attachment; filename=bundle.tar.gz')).toBe('bundle.tar.gz');
  });

  it('strips any directory component', () => {
    // `a.download` ignores paths, but a name that cannot contain one is easier to reason about.
    expect(filenameFromDisposition('attachment; filename="../../etc/passwd"')).toBe('passwd');
    expect(filenameFromDisposition('attachment; filename="C:\\tmp\\x.tar.gz"')).toBe('x.tar.gz');
  });

  it('returns null when there is nothing usable, so the caller picks its own name', () => {
    for (const header of [null, '', 'attachment', 'inline', 'attachment; filename=""', 'attachment; filename=".."']) {
      expect(filenameFromDisposition(header)).toBeNull();
    }
  });
});
