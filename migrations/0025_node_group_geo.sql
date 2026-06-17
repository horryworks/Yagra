-- Optional geo coordinates on node groups, for the dashboard geo-map widget.
--
-- Additive (expand-contract, ADR-017): two nullable columns only. A group with both set is
-- plotted as a pin (colored by its worst member state); groups without coordinates are simply
-- not plotted. No effect on existing behavior.

ALTER TABLE node_groups ADD COLUMN IF NOT EXISTS latitude  DOUBLE PRECISION;
ALTER TABLE node_groups ADD COLUMN IF NOT EXISTS longitude DOUBLE PRECISION;
