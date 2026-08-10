-- 0051_forward_flow_and_tls — flow forwarding + syslog over TLS (ADR-034 Increment 2).
-- Additive only (ADR-017 expand-contract): the CHECK constraints on `source_kind`/`dest_kind` are
-- *widened*, and one nullable column is added. Every value an N-1 core can write still passes, so a
-- rolling upgrade is safe in both directions — a standby still running Increment 1 simply never
-- writes 'flow'/'syslog_tls'/'flow_udp', and rows it cannot interpret are already handled (the
-- store's `row_to_dest` falls back rather than failing the whole query).
--
-- WHY FLOW NEEDED A SECOND INCREMENT. Syslog and traps reach core as `EventMsg`, so attaching the
-- original datagram was a field addition. Flow does not: the poller aggregates at the edge (1-minute
-- buckets, top-N by bytes, identical 5-tuples folded — ADR-031), and that transform is irreversible.
-- Increment 2 therefore adds a second poller→core stream, `yagra.flows.raw`, carrying the datagrams
-- verbatim. The aggregate still feeds ClickHouse; the raw relay feeds only the forwarder.
--
-- FLOW FILTER SEMANTICS. A NetFlow v9 / IPFIX datagram is a template plus many records, and records
-- cannot be removed from one without re-encoding it into a different datagram. A flow filter is
-- therefore an **any-record** test: if any record matches, the whole datagram is relayed — including
-- the records that did not match. The WebUI states this next to the filter builder; it is a property
-- of the wire format, not a shortcut.
--
-- FLOW IS VERBATIM-ONLY. There is no rendered form of a template-bound binary export, so `verbatim`
-- must be true for a flow destination (enforced at the API edge, not by a CHECK here — the rule is a
-- source/destination pairing, which SQL would express far less clearly than the validator does).
--
-- reversible: the DROP CONSTRAINTs immediately re-add a WIDER check, which no binary can fail to
-- satisfy. Nothing is removed and no column is retyped (ADR-050 decision 7).

-- Widen the stream selector: 'flow' joins syslog and traps.
ALTER TABLE forward_destinations DROP CONSTRAINT IF EXISTS forward_destinations_source_kind_check;
ALTER TABLE forward_destinations ADD CONSTRAINT forward_destinations_source_kind_check
    CHECK (source_kind IN ('syslog', 'trap', 'flow'));

-- Widen the transport selector: 'syslog_tls' (RFC 5425) and 'flow_udp'.
ALTER TABLE forward_destinations DROP CONSTRAINT IF EXISTS forward_destinations_dest_kind_check;
ALTER TABLE forward_destinations ADD CONSTRAINT forward_destinations_dest_kind_check
    CHECK (dest_kind IN ('syslog_udp', 'syslog_tcp', 'syslog_tls', 'snmp_trap_udp', 'flow_udp'));

-- PEM-encoded certificate(s) trusted for a TLS destination, in addition to the container's system
-- roots. NOT a secret (a CA certificate is public by construction), so it is stored in the clear
-- rather than in the sealed columns — putting it there would imply a confidentiality property it
-- does not have. NULL = trust the system roots only. Certificate verification is never disabled:
-- there is deliberately no "insecure" flag, because forwarding carries credential-bearing log
-- bodies off-box and an unauthenticated peer is exactly the thing TLS is there to prevent.
ALTER TABLE forward_destinations ADD COLUMN IF NOT EXISTS ca_cert TEXT;
