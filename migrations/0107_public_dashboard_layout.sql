-- 0107_public_dashboard_layout — the one board anonymous visitors see (ADR-123 決定 3).
--
-- reversible: additive only — one new table, nothing added to or narrowed on an existing one. An
-- older core never reads it, so rolling the binary back leaves the row in place; what is lost is
-- the public board itself, and that older binary has no public board to show. No `schema_compat`
-- floor, for the same reason 0104 / 0105 / 0106 record — the floor 0080 recorded covers this one.
--
-- WHY IT IS NOT `shared_dashboard` (0031)
-- The shared board is what every signed-in user lands on, and it is composed for colleagues: it
-- may carry the audit widget (`view_audit`, 403 to anonymous) or flow widgets (a typed 503 when
-- the flow tier is off). Serving it to the public would ⑴ put a board composed for internal use in
-- front of strangers and ⑵ break widgets in a way an admin editing it can never see, because the
-- admin's own session answers every one of those calls. So the public board is composed
-- separately, and the switch (0106) is turned on only after someone has looked at it.
--
-- IT IS ALSO THE ACCESS-CONTROL LIST, WHICH IS THE PART THAT IS EASY TO MISS
-- ADR-123 決定 5: the set of API routes open to an anonymous caller is derived from the widgets
-- this layout carries. Removing a widget from this board closes the routes it read, within one
-- refresh period. So an edit here is not only a presentation change — it changes what an
-- unauthenticated request can reach. That is why the write takes `manage_system` (Admin) rather
-- than the `manage_config` its shared-board sibling takes, and why the row records who saved it.
--
-- No row is seeded: until an admin saves one, GET returns NULL and the WebUI renders its default
-- layout — the same contract as `shared_dashboard`. ⚠️ With no row saved there are no widgets, so
-- the derived route set is empty and an anonymous caller reaches nothing but the three always-open
-- endpoints. That is the correct fail-closed direction: an unconfigured public board shows nothing
-- rather than everything.

CREATE TABLE IF NOT EXISTS public_dashboard (
    -- Singleton: exactly one row, id is always TRUE. Same shape as `shared_dashboard`.
    id          BOOLEAN     PRIMARY KEY DEFAULT TRUE CHECK (id),
    -- Opaque to the backend — the WebUI owns and migrates the shape (same contract as
    -- `user_dashboards` / `shared_dashboard`). ⚠️ Core does read the widget *types* out of it to
    -- build the anonymous route allow-list, but it never interprets a widget's settings.
    layout_json JSONB       NOT NULL,
    -- Username of the admin who last saved. Not merely attribution: this row decides what an
    -- anonymous request can read, so "who last widened it" is an access-control question.
    updated_by  TEXT,
    updated_at  TIMESTAMPTZ NOT NULL DEFAULT now()
);
