-- 0085_interface_link_mode — the physical link's negotiated mode (ADR-063 Inc.1).
--
-- Interface *attributes*, not time series — the exact precedent is `if_speed` and the four optical
-- dBm bounds migration 0084 added: all device-reported, all slow-moving, all read back to render a
-- cell rather than a curve. Storing them in VictoriaMetrics every poll cycle would push ADR-011's
-- cardinality axis by two series per port to say something that changes only when someone re-cables.
--
-- Both are nullable and are written by the same interface-metadata walk that writes `if_name` /
-- `if_alias` / `if_speed`, through the `ON CONFLICT DO UPDATE` that COALESCEs every column against
-- its existing value — so this walk and the optical probe keep writing disjoint columns of the same
-- row without clobbering each other.
--
-- if_duplex — 'half' | 'full', the `Duplex` enum's token (yagra-common/src/link_mode.rs). Read from
-- EtherLike-MIB `dot3StatsDuplexStatus` (1.3.6.1.2.1.10.7.2.1.19).
--
--   NULL means "not known", and that deliberately collapses three cases into one: the MIB is not
--   implemented, the row is absent because the port is down, and the agent answered `unknown(1)`.
--   Nothing consumes the distinction — the UI renders all three as "—" — and inventing a stored
--   'unknown' token would be a fourth thing to keep in step with the enum for no reader.
--
--   ⚠️ Expect NULL on optical ports and do not read it as a fault. IEEE 802.3 defines no half duplex
--   above 1 Gbit/s (the MAU registry's HD/FD suffixes stop at 1000BaseTFD), so an agent that
--   answers `unknown(1)` for a 10G port is being accurate. The column earns its place on copper,
--   where a duplex mismatch is a real and common misconfiguration.
--
-- if_type — IANAifType, stored as the raw integer (ethernetCsmacd = 6, softwareLoopback = 24,
--   propVirtual = 53, ppp = 23, tunnel = 131, …). Deliberately NOT transcribed into an enum: it is
--   an IANA registry of ~300 values that grows, and the only question the product asks of it is
--   "does a duplex/media cell apply to this interface at all?" — one predicate over a handful of
--   named constants, not 300 variants and 600 locale strings.
--
-- reversible: additive nullable columns only. An older core SELECTs its columns by name and never
-- sees these, so a rollback to any earlier release runs unchanged against this schema — nothing
-- reads them, and the values simply sit unused until a newer core is put back.

ALTER TABLE interfaces
    ADD COLUMN if_duplex TEXT,
    ADD COLUMN if_type   INTEGER;
