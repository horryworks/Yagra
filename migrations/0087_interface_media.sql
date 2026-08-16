-- 0087_interface_media — the port's physical media type (ADR-063 Inc.2).
--
-- The third and last of the physical-layer attributes, after `if_speed` (0001) and the duplex /
-- ifType pair (0085). Same tier as those and as the optical bounds (0084): device-reported,
-- slow-moving, written by the same COALESCEing upsert so several probes fill disjoint columns of one
-- row, and read back to render a cell rather than a curve. Nothing here is a time series.
--
-- ── interfaces ──────────────────────────────────────────────────────────────
--
-- if_media — the canonical IEEE designation: '1000BASE-T', '1000BASE-SX', '10GBASE-SR'.
--
--   TEXT and not an enum or a lookup table, deliberately. `dot3MauType` is an IANA registry of
--   250-and-growing designations whose English and Japanese renderings are byte-identical — an enum
--   would buy 250-arm exhaustive matches and ~500 locale keys with nothing to translate. The
--   cardinality objection does not apply either: this is a PostgreSQL attribute beside `if_name`,
--   never a TSDB label (ADR-011).
--
--   Only designations the poller's transcribed table carries are stored; an unrecognised
--   registration is NULL and its sub-identifier is logged, so a gap is discoverable from a running
--   deployment rather than showing an operator the wrong medium.
--
-- transceiver_model — the vendor's own part string for a pluggable, e.g. 'SFP-1000BaseLX'.
--
--   ⚠️ **A separate column because it is a different fact, not a fallback spelling of the same one.**
--   ENTITY-MIB answers with a part number, which is not a media type; coercing one into `if_media`
--   would be the class of lie ADR-062 refused when it dropped implausible optical readings rather
--   than storing them. It populates `if_media` only when it contains a canonical designation as a
--   whole token, and otherwise stands on its own — still useful to an operator, not pretending to be
--   an enum. It is NULL for every fixed copper port, which have no pluggable to describe.
--
-- ── app_settings ────────────────────────────────────────────────────────────
--
-- On by default, like the neighbour (0062), interface-address (0065) and routing (0071) walks, and
-- unlike the ARP walk (0070) which reads a table sized by the network. This one reads one row per
-- Ethernet port on the device itself, once an hour.
--
-- The hour matches those three, for the reason media has in common with them: it changes only when
-- someone swaps a module or re-cables. Riding the interface-metric interval would walk `ifMauTable`
-- on a 48-port switch every minute to learn nothing. Same band as the others, so the one validator
-- at the API edge covers this too; the CHECK is the backstop, not the primary guard.
--
-- reversible: additive nullable columns and two additive settings columns with defaults. An older
-- core SELECTs its columns by name and never sees these, so a rollback to any earlier release runs
-- unchanged — the values sit unused until a newer core is put back.

ALTER TABLE interfaces
    ADD COLUMN if_media          TEXT,
    ADD COLUMN transceiver_model TEXT;

ALTER TABLE app_settings
    ADD COLUMN IF NOT EXISTS media_discovery_enabled BOOLEAN NOT NULL DEFAULT TRUE;

ALTER TABLE app_settings
    ADD COLUMN IF NOT EXISTS media_interval_secs INTEGER NOT NULL DEFAULT 3600
        CHECK (media_interval_secs BETWEEN 300 AND 86400);
