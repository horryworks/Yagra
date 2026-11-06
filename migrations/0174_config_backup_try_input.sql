-- 0174_config_backup_try_input - Yagra, not the operator, picks SSH or telnet for each login
-- attempt, and remembers the one that worked (ADR-242).
--
-- reversible: two nullable columns, and `input` rewritten to a token every release since 0152
-- already knows. An older core reads both tables through explicit column lists, so it never selects
-- the new columns; it reads `ssh_then_telnet` as before and hands oxidized "ssh, telnet", which is
-- the behaviour this release replaces, not a broken one. No `schema_compat` floor, for the reason
-- 0108 records.
--
-- `try_input` is the protocol of the next attempt while a node looks for a login, and the protocol
-- that worked once it has one; NULL means SSH. `config_backup_attempts.input` is the protocol one
-- attempt used; NULL for every attempt recorded before this migration.
ALTER TABLE node_config_backup ADD COLUMN try_input TEXT;
ALTER TABLE config_backup_attempts ADD COLUMN input TEXT;

-- A node whose login already works keeps the protocol it works over (ADR-242 decision 6).
UPDATE node_config_backup SET try_input = input
 WHERE login_confirmed AND input IN ('ssh', 'telnet');

-- Every node moves to the automatic search; "SSH only" is now a box a person ticks again.
UPDATE node_config_backup SET input = 'ssh_then_telnet' WHERE input IN ('ssh', 'telnet');
