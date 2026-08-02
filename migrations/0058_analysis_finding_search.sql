-- 0058_analysis_finding_search — indexes for the cross-run findings search.
--
-- Additive / expand-only (ADR-017): two indexes, no table or column change, so an older core keeps
-- working against this schema unchanged.
--
-- Until now a finding was only ever read through its job (`analysis_findings_job_idx` on
-- `(job_id, score DESC)`), because the only reader was `GET /analysis/jobs/{id}/findings`. The
-- Saved-findings screen reads the other way round — "everything the analyses have found lately,
-- across runs" — which orders by time and pages with a keyset cursor, and neither of those is
-- served by an index on `job_id`.
--
-- The ordering key is `(created_at DESC, id DESC)` and not `created_at` alone. A run inserts its
-- findings in a tight loop, so several rows routinely land inside the same millisecond; a cursor on
-- the timestamp alone would then either repeat or silently skip the rows sharing it. The id is the
-- tiebreak that makes the cursor total, and it has to be in the index for the ordering to come out
-- of the scan rather than a sort.

-- The default listing: newest first, keyset-paged on (created_at, id).
CREATE INDEX IF NOT EXISTS analysis_findings_created_idx
    ON analysis_findings (created_at DESC, id DESC);

-- "What has been found about this node?" — the node-filtered variant of the same order. Partial on
-- `node_id IS NOT NULL` because a fleet-level finding (the flow-tier-off notice, a summary row)
-- never matches a node filter, and there is no point indexing rows the predicate cannot select.
CREATE INDEX IF NOT EXISTS analysis_findings_node_created_idx
    ON analysis_findings (node_id, created_at DESC, id DESC)
    WHERE node_id IS NOT NULL;
