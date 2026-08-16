-- 0084_interface_optical_limits — the transceiver's own acceptable power window (ADR-062 Inc.4).
--
-- These are interface *attributes*, not time series, which is why they live here and not in the
-- TSDB: the exact precedent is `if_speed` two columns up — also device-reported, also slow-moving,
-- and also read back to draw a reference line on a chart. Storing a constant in VictoriaMetrics
-- every poll cycle would push ADR-011's cardinality axis by four series per optical port to say
-- something that changes only when someone swaps a module.
--
-- All four are nullable and stay NULL for every interface that is not optical, which is almost all
-- of them. The optical probe writes them through the same `ON CONFLICT DO UPDATE` the interface
-- metadata walk uses, and that statement COALESCEs every column against its existing value — so
-- the two walks write disjoint columns of the same row without clobbering each other.
--
-- dBm, as reported by the module after the poller has normalised the vendor's scaling. Negative is
-- normal. DOUBLE PRECISION rather than an integer of hundredths: the poller already produced an
-- f64, and storing the pre-scaled integer would put vendor knowledge back into the schema.
--
-- reversible: additive nullable columns only. An older core SELECTs its columns by name and never
-- sees these, so a rollback to any earlier release runs unchanged against this schema — nothing
-- reads them, and the values simply sit unused until a newer core is put back.

ALTER TABLE interfaces
    ADD COLUMN rx_power_low_dbm  DOUBLE PRECISION,
    ADD COLUMN rx_power_high_dbm DOUBLE PRECISION,
    ADD COLUMN tx_power_low_dbm  DOUBLE PRECISION,
    ADD COLUMN tx_power_high_dbm DOUBLE PRECISION;
