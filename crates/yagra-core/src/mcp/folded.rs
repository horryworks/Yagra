// SPDX-License-Identifier: AGPL-3.0-only
//! The folded-read table: which REST route each branch of a folded tool mirrors, and what it
//! demands (ADR-042 I3a).
//!
//! ADR-042 decision 3 says tools are parameterized rather than transcribed — `get_system_health`
//! answers twelve endpoints behind a `section` argument instead of becoming twelve tools, because a
//! model picks worse from a longer list. That fold creates a problem the ADR did not anticipate:
//! **the endpoints behind one tool do not share a permission.**
//!
//! The measured spread across the routes folded here is `View` ×16, `ManageConfig` ×1,
//! `ManageCredentials` ×1, `ViewAudit` ×1, `AckAlerts` ×1, and two that are deliberately
//! unauthenticated over REST. Picking one permission for the whole tool fails in both directions: a
//! loose choice hands the forwarding topology or the audit log to any viewer, and a strict choice
//! recreates the very gap ADR-042 exists to close.
//!
//! So the permission is **data**, one row per branch, and the tool looks it up before it looks at
//! anything else. ADR-042 decision 2 declined a `Permission` column on the 237-row ledger because
//! nothing could check it; that reasoning holds there and not here — over these 21 rows the
//! permission is a value a test can compare against the REST handler's own extractor, and
//! [`tests::every_folded_read_demands_what_its_rest_route_demands`] does exactly that.
//!
//! The same table drives two more checks, so one edit keeps three things honest:
//! [`tests::every_folded_read_is_claimed_by_its_ledger_line`] (the route exists and the ledger
//! names this tool) and [`tests::every_folded_result_is_free_of_forbidden_keys`] (the response
//! schema carries no secret key). The last one reads the **OpenAPI document** rather than a
//! hand-built instance — see its doc comment for why that is stronger than the instance canary it
//! supplements.

use yagra_common::Permission;

/// One branch of a folded MCP tool.
///
/// Only `tool`, `arg` and `perm` are read at runtime — by [`required_permission`], which every
/// folded tool calls before it touches a store. The rest exist for the guards below, which is the
/// point of writing them down: a claim nothing consumes at runtime is still a claim a test can
/// check against the REST side.
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) struct FoldedRead {
    /// The tool that serves this branch.
    pub tool: &'static str,
    /// The argument value selecting it (`section`/`kind`), or the flag that turns it on.
    pub arg: &'static str,
    /// HTTP method of the REST route it mirrors. All but `run_rca` are `GET`.
    pub method: &'static str,
    /// The REST route, in **ledger form** (`:id`, not `{id}`) so it can be matched against
    /// `route_table::ROUTES` directly.
    pub path: &'static str,
    /// What the caller must hold. `None` means the REST route is deliberately unauthenticated —
    /// the two client-bootstrap reads — in which case the tool still requires `View`, because MCP
    /// has no pre-login state to bootstrap and there is no reason to open a second anonymous
    /// surface.
    pub perm: Option<Permission>,
    /// Why this branch may carry `pool`/`profile`, or `None` to hold it to the stricter rule.
    ///
    /// `INVENTORY_NOISE_KEYS` exists because those ids are noise on a row describing monitored
    /// equipment. On the poller-facing reads they are the *answer*, so the exemption is per-row and
    /// has to be argued in writing rather than assumed from the family.
    pub inventory_ids_ok: Option<&'static str>,
    /// Why this branch may return a field the contract describes as `{}` — an untyped blob the key
    /// check cannot see into — or `None` to require that it returns none.
    ///
    /// Almost nothing needs this. A `serde_json::Value` field annotated `#[schema(value_type = T)]`
    /// is **not** opaque: the document carries `T`'s real shape and the walk follows it, which is
    /// how the `pool` on `run_rca`'s stored evidence was found. Only a genuinely undeclared blob
    /// lands here, and it must say so rather than passing for want of anything to check.
    pub opaque_ok: Option<&'static str>,
}

/// The pool exemption, shared by the five reads whose entire subject is poller assignment.
const POOL_IS_THE_ANSWER: Option<&'static str> = Some(
    "the question this branch answers is which poller owns the work, so the pool is the answer \
     rather than inventory noise",
);

/// Every folded branch ADR-042 I3a ships. **This is also the increment's inventory** — the three
/// guards below read it, and the ledger flip in the same increment covers exactly these paths.
pub(crate) const FOLDED_READS: &[FoldedRead] = &[
    // ── get_system_health(section=…) ─────────────────────────────────────────
    FoldedRead {
        tool: "get_system_health",
        arg: "pollers",
        method: "GET",
        path: "/api/v1/pollers",
        perm: Some(Permission::View),
        inventory_ids_ok: POOL_IS_THE_ANSWER,
        opaque_ok: None,
    },
    FoldedRead {
        tool: "get_system_health",
        arg: "poller_health",
        method: "GET",
        path: "/api/v1/poller-health",
        perm: Some(Permission::View),
        inventory_ids_ok: None,
        opaque_ok: None,
    },
    FoldedRead {
        tool: "get_system_health",
        arg: "pools",
        method: "GET",
        path: "/api/v1/pools",
        perm: Some(Permission::View),
        inventory_ids_ok: None,
        opaque_ok: None,
    },
    FoldedRead {
        tool: "get_system_health",
        arg: "poller_nodes",
        method: "GET",
        path: "/api/v1/pollers/:id/nodes",
        perm: Some(Permission::View),
        inventory_ids_ok: POOL_IS_THE_ANSWER,
        opaque_ok: None,
    },
    FoldedRead {
        tool: "get_system_health",
        arg: "node_assignment",
        method: "GET",
        path: "/api/v1/nodes/:node_id/assignment",
        perm: Some(Permission::View),
        // The inverse of `poller_nodes`, and filed beside it rather than on `get_node_status`
        // for a reason that is not cosmetic: `NodeStatusDto` is an *inventory* DTO held to the
        // strict key rule, so hanging an answer containing `pool` off it would have meant either
        // breaking that invariant or wrapping the result to dodge it. Ownership is a
        // poller-topology question; this is where the rest of that family lives.
        inventory_ids_ok: POOL_IS_THE_ANSWER,
        opaque_ok: None,
    },
    FoldedRead {
        tool: "get_system_health",
        arg: "monitoring_gaps",
        method: "GET",
        path: "/api/v1/monitoring-gaps",
        perm: Some(Permission::View),
        inventory_ids_ok: POOL_IS_THE_ANSWER,
        opaque_ok: None,
    },
    FoldedRead {
        tool: "get_system_health",
        arg: "dependencies",
        method: "GET",
        path: "/api/v1/system-health",
        perm: Some(Permission::View),
        inventory_ids_ok: None,
        opaque_ok: None,
    },
    FoldedRead {
        tool: "get_system_health",
        arg: "hosts",
        method: "GET",
        path: "/api/v1/system/hosts",
        perm: Some(Permission::View),
        inventory_ids_ok: POOL_IS_THE_ANSWER,
        opaque_ok: None,
    },
    FoldedRead {
        tool: "get_system_health",
        arg: "host_trends",
        method: "GET",
        path: "/api/v1/system/hosts/:instance/metrics/range",
        perm: Some(Permission::View),
        inventory_ids_ok: None,
        opaque_ok: None,
    },
    FoldedRead {
        tool: "get_system_health",
        arg: "forwarding",
        method: "GET",
        path: "/api/v1/forwarding/status",
        // The one `ManageConfig` member of this family. It names every collector the deployment
        // tees to, which is closer to the forwarding configuration than to a health counter.
        perm: Some(Permission::ManageConfig),
        inventory_ids_ok: None,
        opaque_ok: None,
    },
    FoldedRead {
        tool: "get_system_health",
        arg: "credentials",
        method: "GET",
        path: "/api/v1/credentials/health",
        perm: Some(Permission::ManageCredentials),
        inventory_ids_ok: None,
        opaque_ok: None,
    },
    FoldedRead {
        tool: "get_system_health",
        arg: "version",
        method: "GET",
        path: "/api/v1/version",
        perm: None,
        inventory_ids_ok: None,
        opaque_ok: None,
    },
    FoldedRead {
        tool: "get_system_health",
        arg: "deployment",
        method: "GET",
        path: "/api/v1/config",
        perm: None,
        inventory_ids_ok: None,
        opaque_ok: None,
    },
    // ── the single-purpose I3a tools ─────────────────────────────────────────
    FoldedRead {
        tool: "fleet_state_history",
        arg: "",
        method: "GET",
        path: "/api/v1/fleet/state-history",
        perm: Some(Permission::View),
        inventory_ids_ok: None,
        opaque_ok: None,
    },
    FoldedRead {
        tool: "get_report_runs",
        arg: "list",
        method: "GET",
        path: "/api/v1/reports/runs",
        perm: Some(Permission::View),
        inventory_ids_ok: None,
        opaque_ok: None,
    },
    FoldedRead {
        tool: "get_report_runs",
        arg: "detail",
        method: "GET",
        path: "/api/v1/reports/runs/:id",
        perm: Some(Permission::View),
        inventory_ids_ok: None,
        // `ReportRunDetail.result_json` is the one genuinely untyped blob on this surface: the
        // section renderers decide its keys, so the contract describes it as `{}` and the walk
        // below can say nothing about it. Kept rather than stripped, because the structured result
        // is the whole reason an AI client would read a saved run at all — the HTML is worse. What
        // bounds it is that the renderers are internal and none of them reads a credential: the
        // report catalog is a fixed list of aggregate sections over metrics, alerts and flow.
        opaque_ok: Some(
            "result_json is rendered section output whose keys the internal report renderers \
             decide; no renderer reads credential storage, and the structured result is the reason \
             the run is worth reading",
        ),
    },
    FoldedRead {
        tool: "get_audit",
        arg: "",
        method: "GET",
        path: "/api/v1/audit",
        // `ViewAudit`, not `View` — who acked at 3am is its own permission.
        perm: Some(Permission::ViewAudit),
        inventory_ids_ok: None,
        opaque_ok: None,
    },
    FoldedRead {
        tool: "get_dns_chain",
        arg: "current",
        method: "GET",
        path: "/api/v1/nodes/:node_id/dns-chain",
        perm: Some(Permission::View),
        inventory_ids_ok: None,
        opaque_ok: None,
    },
    FoldedRead {
        tool: "get_dns_chain",
        arg: "history",
        method: "GET",
        path: "/api/v1/nodes/:node_id/dns-chain/history",
        perm: Some(Permission::View),
        inventory_ids_ok: None,
        opaque_ok: None,
    },
    FoldedRead {
        tool: "run_rca",
        arg: "",
        // The only non-GET row: a read wearing POST, like `POST /analysis/jobs`. It changes no
        // configuration, so it does not touch the frozen write surface (ADR-042 decision 6).
        method: "POST",
        path: "/api/v1/rca",
        perm: Some(Permission::AckAlerts),
        // Found by the schema walk, not predicted: the stored evidence embeds `NodeFacts`, which
        // carries the node's pool. Deliberately *not* the `POOL_IS_THE_ANSWER` reason — nobody
        // asked this tool which poller owns anything. The argument is different, so the sentence
        // is different: a saved report is replayed verbatim, the same bytes the UI shows, and
        // rewriting it on one surface would make the two disagree about what was analysed.
        inventory_ids_ok: Some(
            "a stored report is replayed verbatim on both surfaces; its evidence names the node's \
             pool as recorded at generation time, and rewriting that for MCP alone would make the \
             two surfaces disagree about what the model was shown",
        ),
        opaque_ok: None,
    },
    // ── branches folded into tools that already existed ──────────────────────
    FoldedRead {
        tool: "get_fleet_summary",
        arg: "coverage",
        method: "GET",
        path: "/api/v1/fleet/coverage",
        perm: Some(Permission::View),
        inventory_ids_ok: None,
        opaque_ok: None,
    },
];

/// The permission one branch demands, or `Permission::View` for the two whose REST counterpart is
/// unauthenticated (see [`FoldedRead::perm`]).
///
/// Panics if `arg` is not a branch of `tool` — the caller is a `match` in the same crate whose arms
/// are pinned to this table by [`tests::every_folded_branch_is_reachable_from_its_tool`], so an
/// unknown pair is a programming error rather than caller input.
pub(crate) fn required_permission(tool: &str, arg: &str) -> Permission {
    FOLDED_READS
        .iter()
        .find(|f| f.tool == tool && f.arg == arg)
        .unwrap_or_else(|| panic!("no folded read `{tool}`/`{arg}`"))
        .perm
        .unwrap_or(Permission::View)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;

    /// Keys that must never appear in a tool result, at any depth (ADR-018). Kept in step with
    /// `dto.rs`'s copy by [`the_two_key_lists_agree_with_the_canary`].
    const SECRET_KEYS: &[&str] = &[
        "credential",
        "community",
        "password",
        "token",
        "auth_key",
        "priv_key",
        "secret",
    ];
    const INVENTORY_NOISE_KEYS: &[&str] = &["pool", "profile"];

    /// The ledger path (`:id`) for an OpenAPI path (`{id}`) — the same normalization
    /// `route_table::documented` uses.
    fn ledger_path(openapi_path: &str) -> String {
        openapi_path
            .split('/')
            .map(
                |seg| match seg.strip_prefix('{').and_then(|s| s.strip_suffix('}')) {
                    Some(name) => format!(":{name}"),
                    None => seg.to_owned(),
                },
            )
            .collect::<Vec<_>>()
            .join("/")
    }

    /// The OpenAPI document as JSON, so schemas can be walked with `serde_json` rather than by
    /// pattern-matching utoipa's `RefOr<Schema>` tree.
    fn document() -> serde_json::Value {
        serde_json::to_value(crate::api::openapi::document())
            .expect("the OpenAPI document serializes")
    }

    /// The operation object for one folded row, found by ledger path + method.
    fn operation_of<'a>(
        doc: &'a serde_json::Value,
        f: &FoldedRead,
    ) -> Option<&'a serde_json::Value> {
        let paths = doc.get("paths")?.as_object()?;
        let (_, item) = paths.iter().find(|(p, _)| ledger_path(p) == f.path)?;
        item.get(f.method.to_lowercase())
    }

    /// What a walk of one response schema found.
    #[derive(Default)]
    struct Walked {
        /// Every property name reachable from the schema, at any depth.
        keys: BTreeSet<String>,
        /// Properties the contract describes as `{}` — no type, no `$ref`, nothing. The key check
        /// is blind inside these, so they are reported rather than skipped.
        opaque: BTreeSet<String>,
    }

    /// Walk a response schema, following `$ref` into `components.schemas`. Cycles are cut by the
    /// visited set.
    fn walk_schema(doc: &serde_json::Value, schema: &serde_json::Value) -> Walked {
        let mut out = Walked::default();
        let mut seen: BTreeSet<String> = BTreeSet::new();
        walk(doc, schema, &mut seen, &mut out, 0);
        out
    }

    /// Whether a schema says nothing at all about its value.
    fn is_opaque(schema: &serde_json::Value) -> bool {
        schema
            .as_object()
            .is_some_and(|o| !o.keys().any(|k| k != "description" && k != "title"))
    }

    fn walk(
        doc: &serde_json::Value,
        schema: &serde_json::Value,
        seen: &mut BTreeSet<String>,
        out: &mut Walked,
        depth: usize,
    ) {
        if depth > 16 {
            return;
        }
        if let Some(r) = schema.get("$ref").and_then(|v| v.as_str()) {
            let name = r.rsplit('/').next().unwrap_or(r).to_owned();
            if !seen.insert(name.clone()) {
                return;
            }
            if let Some(target) = doc.pointer(&format!("/components/schemas/{name}")) {
                walk(doc, target, seen, out, depth + 1);
            }
            return;
        }
        if let Some(props) = schema.get("properties").and_then(|v| v.as_object()) {
            for (k, v) in props {
                out.keys.insert(k.clone());
                if is_opaque(v) {
                    out.opaque.insert(k.clone());
                }
                walk(doc, v, seen, out, depth + 1);
            }
        }
        for key in ["items", "additionalProperties"] {
            if let Some(v) = schema.get(key) {
                if v.is_object() {
                    walk(doc, v, seen, out, depth + 1);
                }
            }
        }
        for key in ["allOf", "anyOf", "oneOf"] {
            if let Some(arr) = schema.get(key).and_then(|v| v.as_array()) {
                for v in arr {
                    walk(doc, v, seen, out, depth + 1);
                }
            }
        }
    }

    /// **Every folded branch mirrors a route the ledger says this tool serves.**
    ///
    /// Catches the drift where a branch is renamed, or points at a path that no longer exists, or
    /// is served by a tool other than the one the ledger credits — any of which would leave the
    /// permission and canary rows below checking the wrong endpoint.
    #[test]
    fn every_folded_read_is_claimed_by_its_ledger_line() {
        use crate::api::route_table::ROUTES;
        for f in FOLDED_READS {
            let row = ROUTES
                .iter()
                .find(|(m, p, _, _)| *m == f.method && *p == f.path)
                .unwrap_or_else(|| {
                    panic!(
                        "folded read `{}`/`{}` names {} {}, which the route ledger does not list",
                        f.tool, f.arg, f.method, f.path
                    )
                });
            let named = match row.3 {
                crate::api::route_table::Mcp::Tool(name) => name,
                _ => panic!(
                    "{} {} is folded into `{}` but its ledger line is not Mcp::Tool — flip it and \
                     lower MCP_PENDING in the same commit",
                    f.method, f.path, f.tool
                ),
            };
            assert_eq!(
                named, f.tool,
                "{} {} is folded into `{}` but the ledger credits `{named}`",
                f.method, f.path, f.tool
            );
        }
    }

    /// **Every folded branch demands what its REST counterpart demands.**
    ///
    /// This is the check ADR-042 decision 2 said could not be written. It could not be written for
    /// the whole ledger — 237 rows, where the permission lives inside handler bodies and MCP tool
    /// bodies in two different spellings. Over the folded rows it can be, because the tool's side
    /// is a value in this file and the REST side is an extractor in a signature:
    ///
    /// `path` → the document's `operationId` (which is the handler's function name) → `async fn
    /// <name>(` in `api/*.rs` → the `Require*` type in its argument list.
    ///
    /// Skips a row it cannot parse and asserts a floor on how many it compared, so "the parser
    /// stopped matching" cannot read as "everything agrees" — the discipline
    /// `openapi.rs::every_documented_body_is_the_type_its_handler_returns` established.
    #[test]
    fn every_folded_read_demands_what_its_rest_route_demands() {
        let doc = document();
        let sources: Vec<(String, String)> =
            std::fs::read_dir(concat!(env!("CARGO_MANIFEST_DIR"), "/src/api"))
                .expect("api module directory")
                .filter_map(Result::ok)
                .filter(|e| e.path().extension().is_some_and(|x| x == "rs"))
                .filter_map(|e| {
                    let name = e.file_name().to_string_lossy().into_owned();
                    std::fs::read_to_string(e.path()).ok().map(|s| (name, s))
                })
                .collect();

        let marker = |p: Permission| match p {
            Permission::View => "RequireView",
            Permission::AckAlerts => "RequireAckAlerts",
            Permission::ManageMaintenance => "RequireManageMaintenance",
            Permission::ManageConfig => "RequireManageConfig",
            Permission::ManageCredentials => "RequireManageCredentials",
            Permission::ManageUsers => "RequireManageUsers",
            Permission::ViewAudit => "RequireViewAudit",
        };

        let mut compared = 0usize;
        for f in FOLDED_READS {
            let Some(op) = operation_of(&doc, f) else {
                continue;
            };
            let Some(handler) = op.get("operationId").and_then(|v| v.as_str()) else {
                continue;
            };
            // A row whose REST side is deliberately unauthenticated: the document must say so,
            // rather than the table quietly claiming an exemption nobody granted.
            if f.perm.is_none() {
                let public = op
                    .get("security")
                    .and_then(|v| v.as_array())
                    .is_some_and(|a| {
                        a.len() == 1 && a[0].as_object().is_some_and(|o| o.is_empty())
                    });
                assert!(
                    public,
                    "folded read `{}`/`{}` claims {} {} needs no permission, but the contract does \
                     not mark that operation as unauthenticated",
                    f.tool, f.arg, f.method, f.path
                );
                compared += 1;
                continue;
            }
            let needle = format!("async fn {handler}(");
            let Some(sig) = sources.iter().find_map(|(_, src)| {
                let after = src.split(&needle).nth(1)?;
                after.split(')').next()
            }) else {
                continue;
            };
            let want = marker(f.perm.expect("checked above"));
            assert!(
                sig.contains(want),
                "folded read `{}`/`{}` demands {:?}, but its REST handler `{handler}` takes no \
                 `{want}` — one of the two is wrong, and if it is the table then MCP is either \
                 leaking this read or refusing a caller the UI would serve",
                f.tool,
                f.arg,
                f.perm.expect("checked above")
            );
            compared += 1;
        }
        assert!(
            compared >= 19,
            "only compared {compared} folded reads against their handlers; the parser drifted"
        );
    }

    /// **No folded branch can return a secret, checked against the contract rather than a sample.**
    ///
    /// The instance canary in `dto.rs` proves a *particular value* carries no forbidden key. That
    /// is weaker than it looks: a hand-built instance with `None` in an `Option<Inner>` hides every
    /// field of `Inner`, and the cost of building one is 8–25 lines per shape — which is why I3 was
    /// deferred once as "~20 hand-written instances".
    ///
    /// Walking the OpenAPI schema instead costs nothing per shape and sees every field whether or
    /// not a sample would have populated it. It is not a replacement: a `serde_json::Value` field
    /// is an empty schema, so the two types that have one keep their runtime instance. It is the
    /// stronger half everywhere else.
    #[test]
    fn every_folded_result_is_free_of_forbidden_keys() {
        let doc = document();
        let mut checked = 0usize;
        for f in FOLDED_READS {
            let Some(op) = operation_of(&doc, f) else {
                continue;
            };
            let Some(schema) = op.pointer("/responses/200/content/application~1json/schema") else {
                continue;
            };
            let Walked { keys, opaque } = walk_schema(&doc, schema);
            // An untyped blob is the one way this check can be blind. Reporting it turns "the walk
            // found nothing wrong" into "the walk could not look", which is a different sentence
            // and the only one worth trusting.
            match f.opaque_ok {
                None => assert!(
                    opaque.is_empty(),
                    "folded read `{}`/`{}` returns {opaque:?}, which the contract describes as an \
                     untyped blob — the key check cannot see inside it. Give the field a \
                     `#[schema(value_type = …)]` so its real shape is described, or record in \
                     `opaque_ok` why serving an unexaminable value here is acceptable",
                    f.tool,
                    f.arg
                ),
                Some(why) => {
                    assert!(
                        why.len() >= 30,
                        "`{}`/`{}` waves through an untyped blob without a real reason",
                        f.tool,
                        f.arg
                    );
                    assert!(
                        !opaque.is_empty(),
                        "`{}`/`{}` declares an opaque-field exemption but every field is now \
                         described; drop it rather than leaving it to cover a future blob",
                        f.tool,
                        f.arg
                    );
                }
            }
            for bad in SECRET_KEYS {
                assert!(
                    !keys.contains(*bad),
                    "folded read `{}`/`{}` ({} {}) returns a field named {bad:?}; a tool must not \
                     serve it (ADR-018) — give the MCP surface a sanitized DTO",
                    f.tool,
                    f.arg,
                    f.method,
                    f.path
                );
            }
            match f.inventory_ids_ok {
                None => {
                    for bad in INVENTORY_NOISE_KEYS {
                        assert!(
                            !keys.contains(*bad),
                            "folded read `{}`/`{}` returns {bad:?}, which is inventory noise on \
                             anything but a poller-assignment answer; if it *is* the answer here, \
                             say so in `inventory_ids_ok`",
                            f.tool,
                            f.arg
                        );
                    }
                }
                Some(why) => {
                    assert!(
                        why.len() >= 30,
                        "`{}`/`{}` exempts itself from the inventory-id rule without a real reason",
                        f.tool,
                        f.arg
                    );
                    // The exemption must be load-bearing. One granted where the key never appears
                    // is a stale claim that would silently cover a future field.
                    assert!(
                        INVENTORY_NOISE_KEYS.iter().any(|k| keys.contains(*k)),
                        "`{}`/`{}` claims an inventory-id exemption but returns neither `pool` nor \
                         `profile`; drop the exemption rather than leaving it to cover a field \
                         nobody has looked at",
                        f.tool,
                        f.arg
                    );
                }
            }
            checked += 1;
        }
        assert!(
            checked >= 19,
            "only walked {checked} folded response schemas; the parser drifted"
        );
    }

    /// The two key lists here and in `dto.rs` are the same rule; nothing but this makes them agree.
    #[test]
    fn the_two_key_lists_agree_with_the_canary() {
        let dto_src = include_str!("dto.rs");
        for k in SECRET_KEYS.iter().chain(INVENTORY_NOISE_KEYS) {
            assert!(
                dto_src.contains(&format!("\"{k}\"")),
                "`{k}` is banned here but absent from dto.rs's lists — one of the two drifted"
            );
        }
    }

    /// Every branch this table declares is one the tool actually dispatches on.
    ///
    /// One-directional on purpose: the reverse (a tool arm with no row) is caught by
    /// [`every_folded_read_is_claimed_by_its_ledger_line`] via the ledger, which cannot be
    /// satisfied without a row here.
    #[test]
    fn every_folded_branch_is_reachable_from_its_tool() {
        let tools_src = include_str!("tools.rs");
        for f in FOLDED_READS.iter().filter(|f| !f.arg.is_empty()) {
            assert!(
                tools_src.contains(&format!("\"{}\"", f.arg)),
                "folded branch `{}`/`{}` appears in no arm of tools.rs — a caller naming it would \
                 be told it is unknown while this table says it exists",
                f.tool,
                f.arg
            );
        }
    }

    #[test]
    fn required_permission_falls_back_to_view_for_the_unauthenticated_reads() {
        assert_eq!(
            required_permission("get_system_health", "version"),
            Permission::View,
            "an unauthenticated REST route still needs View over MCP"
        );
        assert_eq!(required_permission("get_audit", ""), Permission::ViewAudit);
    }
}
