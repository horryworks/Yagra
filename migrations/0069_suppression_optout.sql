-- 0069_suppression_optout — the per-node escape hatch, and when the topology mode last changed
-- (ADR-043 Increment 3).
--
-- Additive only (ADR-017 expand-contract): two new columns with defaults.
--
-- WHY A PER-NODE OPT-OUT DOES NOT REOPEN THE PROBLEM THIS ADR SOLVED
-- ADR-043 exists because per-edge input cost meant the dependency graph was never filled in, so
-- another per-node switch looks like the same mistake. It is not, and the difference is the
-- direction it can fail in: this flag only ever **removes** parents, so a node carrying it alerts
-- more, never less. It cannot create the silent-failure class — an outage nobody is told about —
-- that per-edge approval was rejected for creating.
--
-- What it answers is a question the deployment-wide mode structurally cannot: "whatever else you
-- suppress, this one box must always page us." Without it the only way to express that is to leave
-- the whole fleet on the manual graph.
ALTER TABLE nodes
    ADD COLUMN IF NOT EXISTS suppression_opt_out BOOLEAN NOT NULL DEFAULT FALSE;

-- WHEN THE MODE LAST CHANGED
--
-- The promotion checklist is **advisory and stays advisory**: shadow mode having run for a while is
-- evidence, not a precondition, and enforcing it would put approval back on a per-something
-- schedule. But an operator cannot judge "has this been compared long enough" without knowing when
-- the comparison started, and `updated_at` on the settings row moves for every unrelated setting.
--
-- Nullable rather than defaulted to now(): an existing deployment has genuinely never changed the
-- mode, and stamping the migration's own run time would be a claim about history that is not true.
ALTER TABLE app_settings
    ADD COLUMN IF NOT EXISTS topology_mode_since TIMESTAMPTZ;
