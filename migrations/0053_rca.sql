-- 0053_rca — AI-assisted root-cause analysis (ADR-029 Increment 1): the active LLM provider's
-- configuration, and the reports generated from it.
--
-- Additive only (ADR-017 expand-contract): two new tables, no change to any existing one. An N-1
-- binary never reads either, so a rollback just leaves them orphaned — RCA stops being offered and
-- nothing else notices. There is no contract phase.
--
-- DEFAULT OFF, AND OFF MEANS NO EGRESS
-- Absence of a row in `llm_config` is the off state, not a flag that happens to be false. With no
-- row there is no provider to build, no credential to decrypt and no outbound request — the same
-- shape as `oidc_providers` (0044) and `forward_destinations` (0050). Upgrading a running system
-- therefore changes nothing until an operator deliberately configures a provider and chooses, in
-- that same form, which cloud boundary their incident data may cross.
--
-- WHY llm_config IS A SINGLE ROW
-- ADR-029 fixes ONE active provider selected by configuration — deliberately not a chain with
-- failover, because falling back to a second vendor would silently move hostnames, addresses,
-- topology and syslog across a trust boundary the operator picked on purpose. Storing one row makes
-- that constraint structural (`CHECK (id = 1)`) rather than a rule the application has to remember.
-- The cost is that switching vendors means re-entering the credential; the benefit is that Yagra
-- holds exactly one LLM secret at a time.
--
-- THE SEALED COLUMNS
-- The API key (Gemini / Anthropic) or service-account JSON (Vertex) is envelope-encrypted exactly
-- like every other secret in the schema — ciphertext plus a wrapped DEK, with the KEK outside the
-- database (ADR-018). The five columns are the same set `oidc_providers` and `credentials` carry.
-- They are NULLABLE as a group here, unlike those tables, because Vertex on GKE/GCE authenticates
-- through the instance's own Workload Identity: that configuration legitimately stores no secret at
-- all, which is the *better* deployment and should not be forced to invent a placeholder. The
-- CHECK enforces all-five-or-none so a half-written seal can never be read back as a key.
--
-- WHY THE REPORTS ARE PERSISTED
-- A generated RCA is worth keeping for two independent reasons. Operationally, an explanation that
-- vanishes on page reload is not usable during an incident, and re-deriving it means paying a
-- vendor again for a differently-worded answer to the same question — model output is
-- non-deterministic, and an explanation that changes each time you open it does not earn trust.
-- Institutionally, `body` is the record of what the model was told and what it said, which is what
-- makes a wrong answer reviewable after the fact.
--
-- `context_digest` is a hash of the assembled context, and it is the cache key: the same incident
-- evidence yields the same digest, so a repeat request inside the TTL is served from here without
-- any external call. It hashes the CONTEXT, not the alert id, so an incident that has since gained
-- twenty more dependents correctly misses the cache and gets re-explained.
--
-- No device credentials or secrets reach these rows: the context builder never touches the
-- credential store, and `body` holds only what was already shown in the UI (security.md).

CREATE TABLE IF NOT EXISTS llm_config (
    -- Single row by construction: ADR-029's "one active provider" as a table constraint.
    id                SMALLINT PRIMARY KEY DEFAULT 1 CHECK (id = 1),
    -- 'vertex' | 'gemini' | 'claude' (yagra_core::rca::ProviderKind tokens). Free text rather than
    -- an enum type: an unknown value is rejected at the API edge, and a CHECK here would need a
    -- migration to add the next vendor.
    provider          TEXT NOT NULL,
    -- Operator-entered model id. Deliberately unvalidated against any list — model ids turn over far
    -- faster than Yagra releases, and a hardcoded allowlist would age into a bug.
    model             TEXT NOT NULL,
    -- Vertex only: the GCP project billed, and the region the data stays in. Empty for the others.
    project           TEXT NOT NULL DEFAULT '',
    location          TEXT NOT NULL DEFAULT '',
    enabled           BOOLEAN NOT NULL DEFAULT FALSE,
    max_output_tokens INTEGER NOT NULL DEFAULT 8192 CHECK (max_output_tokens BETWEEN 256 AND 65536),
    -- Envelope-encrypted credential (ADR-018). All five together, or none at all — see above.
    key_id            INTEGER,
    wrapped_dek       BYTEA,
    dek_nonce         BYTEA,
    ciphertext        BYTEA,
    ct_nonce          BYTEA,
    updated_at        TIMESTAMPTZ NOT NULL DEFAULT now(),
    CONSTRAINT llm_config_secret_all_or_none CHECK (
        (key_id IS NULL AND wrapped_dek IS NULL AND dek_nonce IS NULL
             AND ciphertext IS NULL AND ct_nonce IS NULL)
     OR (key_id IS NOT NULL AND wrapped_dek IS NOT NULL AND dek_nonce IS NOT NULL
             AND ciphertext IS NOT NULL AND ct_nonce IS NOT NULL)
    )
);

CREATE TABLE IF NOT EXISTS rca_reports (
    id             UUID PRIMARY KEY,
    -- The node the incident was attributed to (after the root-cause hop), not necessarily the one
    -- the operator clicked. Cascades: an explanation of a deleted node explains nothing.
    node_id        UUID NOT NULL REFERENCES nodes (id) ON DELETE CASCADE,
    -- The check on that node. Not a foreign key — a check id is an alert-engine identity, not a row.
    check_id       UUID NOT NULL,
    -- Hash of the assembled context; the cache key (see header).
    context_digest TEXT NOT NULL,
    -- Which vendor and model produced this. Kept per row, not read from llm_config, so a report
    -- stays attributable after the operator switches providers.
    provider       TEXT NOT NULL,
    model          TEXT NOT NULL,
    -- The one-line answer, lifted out for list views that should not parse `body`.
    summary        TEXT NOT NULL,
    -- The full report: the parsed sections, or the raw text when the model did not return JSON,
    -- plus the evidence timeline the answer was grounded in.
    body           JSONB NOT NULL,
    generated_at   TIMESTAMPTZ NOT NULL DEFAULT now(),
    -- Who asked. The generation itself is audited by the API middleware; this makes the report
    -- self-describing without a join.
    created_by     TEXT NOT NULL
);

-- The cache lookup: newest report for a digest, then a TTL check in the application.
CREATE INDEX IF NOT EXISTS rca_reports_digest_idx ON rca_reports (context_digest, generated_at DESC);
-- "What have we explained about this node lately" — the node-detail and history views.
CREATE INDEX IF NOT EXISTS rca_reports_node_idx ON rca_reports (node_id, generated_at DESC);
