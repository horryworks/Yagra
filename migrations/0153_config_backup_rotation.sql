-- 0153_config_backup_rotation — config backup tries every CLI login in turn and remembers the one
-- that worked, within limits that keep a device or a shared account from being locked out
-- (ADR-210 stage 3).
--
-- reversible: additive only — six nullable or defaulted columns on `node_config_backup` and one new
-- table. A core from before this migration reads `credential_id` as the node's login, which is
-- still the login handed to oxidized, so rolling the binary back keeps fetching with whatever this
-- one last chose. No `schema_compat` floor, for the reason 0108 records.
--
-- `node_config_backup`, new columns
-- `login_order` is the node's own order of CLI logins; NULL means "every CLI login, in the try order
-- of Credentials & secrets". An array rather than a join table because it is read and written whole
-- and a deleted credential is dropped when it is read, not by a foreign key. `credential_id` keeps
-- its meaning for an older core: the login currently handed to oxidized. `login_confirmed` says
-- that login has fetched successfully. `round_started_at` is when the current pass through the
-- candidates began: a candidate that failed since then is not tried again in this pass, and a pass
-- with no candidate left is over until something restarts it. `requested_at` is when Yagra last
-- asked oxidized to fetch and has heard nothing back. `next_try_at` is only for the screen: when the
-- limits next allow a try. `last_version_oid` tells a new version from an unchanged one.
--
-- `config_backup_attempts` — ONE ROW PER LOGIN ATTEMPT
-- What the limits count (three failures per device per hour, five per login across the fleet per
-- hour), so they hold across a restart and an HA failover. The outcome is a token the Rust enum
-- guards. Kept 30 days. Observed state, not configuration: the configuration bundle does not carry
-- it.
ALTER TABLE node_config_backup
    ADD COLUMN login_order      UUID[],
    ADD COLUMN login_confirmed  BOOLEAN NOT NULL DEFAULT false,
    ADD COLUMN round_started_at TIMESTAMPTZ,
    ADD COLUMN requested_at     TIMESTAMPTZ,
    ADD COLUMN next_try_at      TIMESTAMPTZ,
    ADD COLUMN last_version_oid TEXT;

-- A login a person chose under stage 2 becomes that node's whole order, and one that has already
-- fetched is remembered as working.
UPDATE node_config_backup SET login_order = ARRAY[credential_id] WHERE credential_id IS NOT NULL;
-- A row saved with no login must not become "every login": stage 2 let an account without Reveal
-- credentials, or one restricted to folders, switch a node on without choosing one. An empty order
-- tries nothing, so the node reads as having no login until someone allowed to hand logins out
-- saves it again.
UPDATE node_config_backup SET login_order = '{}' WHERE credential_id IS NULL;
UPDATE node_config_backup b SET login_confirmed = true
FROM config_backup_status s
WHERE s.node_id = b.node_id AND s.status = 'success' AND b.credential_id IS NOT NULL;

CREATE TABLE config_backup_attempts (
    id            BIGSERIAL PRIMARY KEY,
    node_id       UUID NOT NULL REFERENCES nodes (id) ON DELETE CASCADE,
    credential_id UUID REFERENCES credentials (id) ON DELETE SET NULL,
    outcome       TEXT NOT NULL,
    new_version   BOOLEAN,
    at            TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX config_backup_attempts_node_idx ON config_backup_attempts (node_id, at);
CREATE INDEX config_backup_attempts_credential_idx ON config_backup_attempts (credential_id, at);
