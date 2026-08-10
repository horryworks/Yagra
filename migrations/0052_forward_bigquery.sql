-- 0052_forward_bigquery — BigQuery as a forwarding destination (ADR-034 Increment 3).
--
-- Additive only (ADR-017 expand-contract): one widened CHECK, no column changes, no data touched.
-- An N-1 core never writes 'bigquery' so nothing it does is rejected; an N-1 core *reading* a row a
-- newer core wrote falls back to `syslog_udp` in `row_to_dest` and the destination simply does not
-- run there — the same N-1 shape migration 0051 used for `flow`.
--
-- WHY A THIRD INCREMENT. The first two increments relay datagrams: what Yagra received goes back on
-- the wire, byte-for-byte where possible. BigQuery is the other question — not "mirror my syslog to
-- the SIEM" but "let me query six months of it". So it is the one destination kind that produces
-- **normalized structured rows** (one per event, one per flow record) instead of a datagram.
--
-- Three consequences follow from that, and they are why this could not be a variation on `flow_udp`:
--   * `verbatim` must be false. There is no byte-exact form of a table row, and deliberately no
--     raw-payload column — storing the original bytes in BigQuery would make the credential exposure
--     that already worries us (syslog bodies carry passwords) permanent and queryable off-box.
--   * Flow filtering becomes **exact per record**, not the any-record datagram test `flow_udp` is
--     stuck with: rows are independent, so a non-matching record is simply not written.
--   * `target` stops being `host:port` and becomes `project.dataset.table`. The column already
--     accepts it — it was deliberately free-form text, not INET — so no schema change is needed,
--     and the API edge validates the shape per `dest_kind`.
--
-- SECURITY (security.md/ADR-018): a Google service-account key is a credential and reuses the same
-- five sealed columns as the SNMP community — envelope-encrypted with the KEK, never returned by the
-- API, never logged. Leaving it unset selects GCE/GKE Workload Identity instead, which stores no
-- secret at all and is the better deployment where it is available.
--
-- reversible: the DROP CONSTRAINT immediately re-adds a WIDER check, which no binary can fail to
-- satisfy. Nothing is removed and no column is retyped (ADR-050 decision 7).
ALTER TABLE forward_destinations DROP CONSTRAINT IF EXISTS forward_destinations_dest_kind_check;
ALTER TABLE forward_destinations ADD CONSTRAINT forward_destinations_dest_kind_check
    CHECK (dest_kind IN ('syslog_udp', 'syslog_tcp', 'syslog_tls', 'snmp_trap_udp', 'flow_udp',
                         'bigquery'));
