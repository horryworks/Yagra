-- 0063_notification_templates — let a channel override the subject/body Yagra sends (ADR-039).
--
-- `.claude/rules/monitoring-conventions.md` has required templated notification content since the
-- rules were written; the implementation was `format!("node {} is {}", alert.node, alert.state)`
-- with `alert.node` being a UUID, so what an operator actually received was
-- `node 6f1c9d2a-0b3e-4a71-9c8d-2e5f7a1b4c60 is critical`. These two columns are what closes that.
--
-- **Why these are plaintext columns and not part of the sealed config blob.** A channel's
-- connection config is a secret and lives encrypted in `ciphertext` (ADR-018). A template is not a
-- secret — it is operator-authored presentation text, it is returned to the UI for editing, and it
-- goes out in the notification itself. Two further reasons it must not live in the blob: there is
-- no update path for the blob (the repo can only create/enable/delete a channel), and a template
-- edit would otherwise mean re-sealing a DEK to change a subject line.
--
-- **NULL means "use the built-in format", and that is load-bearing.** It is not the same as an
-- empty string, which is a template that renders to nothing — a subject line an operator could set
-- by accident and then wonder where their emails went. Every existing row is NULL, so an upgrade
-- changes no notification by a single byte.
--
-- N-1 (rolling upgrade): additive with no default, so an older core neither selects nor writes
-- these columns and keeps sending the built-in format. A newer core reading a row an older one
-- inserted sees NULL, which is exactly what it means. No bus message changes — notification
-- delivery is entirely inside core.
--
-- The length CHECKs are a **backstop**, not the primary guard: `api/notifications.rs` compiles the
-- template and rejects an oversized or unparseable one at the edge with a typed 400, the same
-- discipline 0061/0062 use. They exist so a direct SQL write cannot store something the renderer
-- would then have to reject at delivery time. The body bound matches
-- `notify_render::MAX_BODY_CHARS`; the subject bound is deliberately looser than
-- `MAX_SUBJECT_CHARS` (512) because that cap is on the *rendered output*, and a template can
-- reasonably be longer than what it produces.

ALTER TABLE notification_channels
    ADD COLUMN IF NOT EXISTS subject_template TEXT
        CHECK (subject_template IS NULL OR length(subject_template) <= 4000),
    ADD COLUMN IF NOT EXISTS body_template TEXT
        CHECK (body_template IS NULL OR length(body_template) <= 64000);
