-- 0047_builtin_trap_rules — seed a starter set of built-in SNMP-trap event rules.
--
-- Additive (ADR-017 expand-contract): only INSERTs into the existing event_rules table, so
-- an N-1 binary is unaffected. These give trap reception useful out-of-the-box behavior
-- (before this, a received trap did nothing until an operator hand-authored a rule).
--
-- Matching relies on core's trap-name enrichment (yagra_common::trap_oid_name): the matcher
-- prepends the resolved MIB name to the text a rule sees, so a `substring 'linkDown'` rule
-- matches regardless of poller version. This migration therefore ships in the same release
-- as that enrichment.
--
-- Seed-ONCE semantics: fixed ids + `ON CONFLICT (id) DO NOTHING`. Built-in event rules are
-- ordinary, user-owned rows — operators can edit, disable, or delete them on the Event Rules
-- page. We deliberately do NOT re-seed/overwrite on re-run (unlike the boot-time MIB catalog),
-- so a customized or deleted built-in stays the operator's way. Changing the built-in set in a
-- future release needs a new migration with new ids.
--
-- All rules are global (node_id NULL, source_id NULL) and scoped to trap events (source_kind
-- 'trap'). The noisy/informational traps are severity 'info' (recorded, never alerted) to
-- avoid a notification storm on upgrade; only the genuinely actionable ones raise an alert,
-- and those pair a clear_pattern so the recovery trap resolves the alert (anti-flap).

INSERT INTO event_rules
    (id, name, enabled, source_kind, source_id, node_id, match_kind, pattern, clear_pattern,
     severity, ttl_secs, min_count, window_secs)
VALUES
    -- Interface down → warning; the matching linkUp trap clears it. Node-level (the event
    -- engine keys alerts per node, not per ifIndex), consistent with the rest of the engine.
    ('b0000001-0000-4000-8000-000000000001', 'Link down (linkDown)', true, 'trap', NULL, NULL,
     'substring', 'linkDown', 'linkUp', 'warning', 3600, 1, 60),
    -- Cold start = full reinitialization (typically an unexpected reboot) → warning.
    ('b0000002-0000-4000-8000-000000000002', 'Device restart (coldStart)', true, 'trap', NULL, NULL,
     'substring', 'coldStart', NULL, 'warning', 1800, 1, 60),
    -- Warm start = agent reinitialized without config change → record only.
    ('b0000003-0000-4000-8000-000000000003', 'Agent restart (warmStart)', true, 'trap', NULL, NULL,
     'substring', 'warmStart', NULL, 'info', 1800, 1, 60),
    -- SNMP authentication failure = a request with a bad community/credential → record only
    -- (frequently benign scanner/misconfig noise; operators can raise it to warning if wanted).
    ('b0000004-0000-4000-8000-000000000004', 'SNMP authentication failure', true, 'trap', NULL, NULL,
     'substring', 'authenticationFailure', NULL, 'info', 1800, 1, 60),
    -- BGP session dropped out of Established → warning; bgpEstablished clears it.
    ('b0000005-0000-4000-8000-000000000005', 'BGP session down (bgpBackwardTransition)', true,
     'trap', NULL, NULL, 'substring', 'bgpBackwardTransition', 'bgpEstablished', 'warning', 3600, 1, 60)
ON CONFLICT (id) DO NOTHING;
