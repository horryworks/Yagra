-- 0145_notification_template_free_layout — a notification template may be laid out (ADR-199).
--
-- reversible: one column is added, NOT NULL with a constant default, so the add is a catalogue
-- change and rewrites no row. A core from before it neither reads nor writes the column. It does
-- render a template saved with the column set as written, line breaks and indentation included,
-- which changes how that notification reads and loses nothing. No `schema_compat` floor, for the
-- reason 0108 records: every release from 0.2.2 on tolerates a database carrying migrations it does
-- not embed.
--
-- WHY
-- A template sends every character it holds, so a template with conditions had to be written on one
-- line to send one line. With `template_free_layout` set, the indentation at the start of each line
-- is not sent, a line holding only tags or comments is not sent, and the subject sends no line
-- break. The default keeps every saved template sending exactly what it sent before.
ALTER TABLE notification_channels
    ADD COLUMN IF NOT EXISTS template_free_layout BOOLEAN NOT NULL DEFAULT FALSE;
