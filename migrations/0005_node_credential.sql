-- 0005_node_credential — bind a monitoring credential to a node (Workstream #2).
--
-- Additive (ADR-017). The poller never reads the credential store; core resolves the
-- bound credential and inlines the decrypted secret into the poll job (ADR-018/020).
-- ON DELETE SET NULL so removing a credential simply unbinds it (no node loss).

ALTER TABLE nodes
    ADD COLUMN credential_id UUID REFERENCES credentials (id) ON DELETE SET NULL;
