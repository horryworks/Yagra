// SPDX-License-Identifier: AGPL-3.0-only
//! The checks that read the MCP tool surface as **text**, and the checks on the prose it publishes
//! (ADR-086).
//!
//! They live together and away from the tools because that is what they are about: the surface as a
//! whole, not any one tool. They also read it through [`crate::mcp::tool_source`], which derives the
//! file list from the directory — see that module for why a hand-written list is refused here.

use super::*;
use crate::mcp::tools::testkit::*;
use yagra_common::Permission;

/// **Every series `get_interface_series` returns is named in the description a client reads.**
///
/// ADR-062 added `rx_power_dbm`/`tx_power_dbm` to the result and touched no description, and
/// nothing in the build could see it. The route ledger only moves when a *route* appears, so
/// its fourth column stayed green — the tool existed and still does. The canary only bans
/// forbidden *keys*, so a new key passing the ban reads as covered. And the description is the
/// one artifact an MCP client actually consults before choosing a tool, which made the failure
/// "the data ships and no model asks for it": present, correct, and unreachable in practice.
///
/// The rule this pins is narrow on purpose — the description **enumerates** the arrays, so an
/// enumeration missing a member is a defect regardless of how well the prose reads.
///
/// ⚠️ **This guards one tool, and extending it would make it worse.** `get_node_status`
/// describes the same kind of payload in prose ("nominal speed", "current load in bits/sec each
/// way"), which is good writing and would fail a field-name scan. Covering it would take an
/// exemption list, and an exemption list is the spelling that requires no thought, so it
/// becomes the only one anyone writes (`extensibility.md`). One tool, held tightly, beats every
/// tool held by a list nobody reads.
///
/// `timestamps` is the single exclusion: the description calls it "one shared timestamp axis",
/// describing the concept rather than naming a series, and that is the right way to say it.
#[test]
fn the_interface_series_description_names_every_series_it_returns() {
    let description =
        crate::api::route_table::declared_mcp_tool_description("get_interface_series")
            .expect("get_interface_series declares a description");

    let json = serde_json::to_value(crate::api::metrics::canary_interface_series())
        .expect("InterfaceSeries serializes");
    let keys: Vec<String> = json
        .as_object()
        .expect("InterfaceSeries serializes to a JSON object")
        .keys()
        .filter(|k| k.as_str() != "timestamps")
        .cloned()
        .collect();

    // Asserted so "the parser stopped matching" cannot masquerade as "everything is named":
    // an empty key list would pass the check below without looking at anything.
    assert!(
        keys.len() >= 10,
        "only {} series keys found on InterfaceSeries — the canary instance or the parser \
             drifted, and a check that inspects nothing passes",
        keys.len()
    );

    let missing: Vec<&String> = keys
        .iter()
        .filter(|k| !description.contains(k.as_str()))
        .collect();
    assert!(
        missing.is_empty(),
        "get_interface_series returns series that its description never names, so a client has \
             no reason to ask for them and no way to read them correctly: {missing:?}"
    );
}

/// The description names every `kind` the tool accepts, and says how many there are correctly.
///
/// The count half is the one that had already rotted: the parameter doc said "28 values" while
/// `NAMES` held 29, because ADR-068 added `discovery_scans` and touched neither string. A
/// number in prose beside a list is a second copy of the list's length, so it is derived here
/// rather than trusted.
///
/// ⚠️ **The name check is substring-based and therefore one-directional.** `discovery_scan` is
/// a substring of `discovery_scans`, so a description naming only the plural would still pass
/// for the singular. It catches an omitted kind, not a mis-stated one.
#[test]
fn the_config_description_names_every_kind_it_accepts() {
    let description = crate::api::route_table::declared_mcp_tool_description("get_config")
        .expect("get_config declares a description");
    let missing: Vec<&&str> = ConfigKind::NAMES
        .iter()
        .filter(|n| !description.contains(**n))
        .collect();
    assert!(
        missing.is_empty(),
        "get_config accepts kinds its description never names, so a client has no way to know \
             they exist: {missing:?}"
    );

    let surface = crate::mcp::tool_source::tool_surface();
    let src: &str = surface.as_str();
    let stated: usize = src
        .split("see the tool description for the ")
        .nth(1)
        .expect("the kind parameter states how many values there are")
        .split(' ')
        .next()
        .expect("a number follows")
        .parse()
        .expect("the stated count is a number");
    assert_eq!(
        stated,
        ConfigKind::NAMES.len(),
        "the kind parameter's doc says {stated} values and ConfigKind::NAMES has {}",
        ConfigKind::NAMES.len()
    );
}

/// The description names **every permission any of its kinds can demand**.
///
/// This is the check that would have caught the sentence as it stood: it said "the rest need
/// manage-config" while `notification_channels`, `routing_rules`, `forward_destinations` and
/// `llm` had demanded `ManageSystem` since ADR-057 split the roles. A description is published
/// to every client **verbatim**, so a wrong one is not a documentation defect — it is a model
/// confidently telling an operator that a Viewer token will read their notification channels.
///
/// The mechanics live in [`a_description_names_exactly_the_permissions_it_demands`], shared with
/// the `get_system_health` twin: the labels come from `folded::required_permission`, the same
/// lookup the tool itself uses at call time, so this compares the prose against the enforcement
/// rather than against a second hand-written list.
#[test]
fn the_config_description_names_every_permission_it_can_demand() {
    let args: Vec<&str> = ConfigKind::NAMES
        .iter()
        .map(|n| ConfigKind::parse(n).expect("every NAME parses").arg())
        .collect();
    a_description_names_exactly_the_permissions_it_demands(
        "get_config",
        "Kinds require different permissions:",
        &args,
        // manage-users / manage-system / view / manage-config.
        4,
    );
}

/// Every filter dimension a shared REST/MCP seam declares is actually passed by the tool.
///
/// **This is the half the compiler cannot see.** A seam struct like
/// `api::alerts::HistoryFilterInput` makes a *new* field a compile error here — the initializer
/// below will not build without it — but the compiler is satisfied by `acked: None`. A
/// dimension that is declared and then hardcoded away is the same silent failure as one that
/// was never declared, and it is the failure ADR-042 read parity exists to prevent: the WebUI
/// can ask the question and `/mcp` quietly cannot.
///
/// One table rather than one test per seam. There were four seams by ADR-053 Inc.4b and the
/// per-seam version had already been written twice; a fifth would have been written a third
/// time, or not at all.
///
/// The two escape hatches each need a stated reason, so an exception is a decision rather than
/// the path of least resistance:
/// - `hardcoded` — the tool deliberately does not offer the dimension.
/// - `renamed` — the tool's parameter has a different name from the seam's field.
#[test]
fn every_shared_filter_seam_is_passed_through_whole() {
    let surface = crate::mcp::tool_source::tool_surface();
    let tools: &str = surface.as_str();
    const ALERTS: &str = include_str!("../../api/alerts.rs");
    const AUDIT: &str = include_str!("../../api/audit.rs");
    const ANALYSIS: &str = include_str!("../../api/analysis.rs");
    const EVENTLOG: &str = include_str!("../../api/eventlog.rs");

    struct Seam {
        src: &'static str,
        decl: &'static str,
        init: &'static str,
        /// `(field, why the tool does not take it)`
        hardcoded: &'static [(&'static str, &'static str)],
        /// `(seam field, the tool's parameter name)`
        renamed: &'static [(&'static str, &'static str)],
    }
    // ⚠️ `crate::thresholds::ThresholdFilter` is deliberately **not** here, and its absence is
    // what let `get_config(kind=thresholds)` pass `&Default::default()` unnoticed until
    // ADR-079. It does not fit this harness — three fields against the `>= 5` floor that keeps
    // a broken parser from passing, and the tool builds it in two steps (params → owned →
    // borrowed) rather than one literal. It is guarded behaviourally instead, by
    // `every_threshold_filter_dimension_is_reachable_from_get_config` below, which is the
    // stronger check: it asserts each parameter moves its own dimension and no other.
    let seams = [
        Seam {
            src: ALERTS,
            decl: "pub(crate) struct HistoryFilterInput<'a> {",
            init: "crate::api::alerts::HistoryFilterInput {",
            hardcoded: &[],
            renamed: &[],
        },
        Seam {
            src: AUDIT,
            decl: "pub(crate) struct AuditFilterInput<'a> {",
            init: "crate::api::audit::AuditFilterInput {",
            hardcoded: &[],
            renamed: &[],
        },
        Seam {
            src: ANALYSIS,
            decl: "pub(crate) struct SavedFindingsQuery {",
            init: "crate::api::analysis::SavedFindingsQuery {",
            hardcoded: &[],
            renamed: &[],
        },
        Seam {
            src: EVENTLOG,
            decl: "pub(crate) struct EventFilterInput<'a> {",
            init: "crate::api::eventlog::EventFilterInput {",
            // `search_events` pages by `before_ts`/`before_id` through its own cursor rather
            // than the log list's single `before`, so there is no parameter to pass here.
            hardcoded: &[("before", "the tool carries its own two-part cursor")],
            // The tool names the whole-row term `search` (it is not a column), and bounds the
            // window with `since`/`until` like every other tool rather than `start`/`end`.
            renamed: &[("start", "since"), ("end", "until"), ("q", "search")],
        },
    ];

    let mut checked = 0;
    for seam in seams {
        let body = seam
            .src
            .split(seam.decl)
            .nth(1)
            .unwrap_or_else(|| panic!("seam struct not found: {}", seam.decl));
        let fields: Vec<&str> = body
            .lines()
            .take_while(|l| !l.starts_with('}'))
            .map(str::trim)
            .filter(|l| !l.starts_with("///") && !l.starts_with("//") && !l.starts_with('#'))
            .filter_map(|l| {
                l.strip_prefix("pub(crate) ")
                    .or_else(|| l.strip_prefix("pub "))
            })
            .filter_map(|l| l.split(':').next())
            .filter(|f| !f.is_empty())
            .collect();
        assert!(
            fields.len() >= 5,
            "field extraction produced {} fields for {} — the struct shape changed and this \
                 test is now checking almost nothing",
            fields.len(),
            seam.decl
        );

        let call = tools
            .split(seam.init)
            .nth(1)
            .unwrap_or_else(|| panic!("no tool builds {}", seam.init));
        let call: Vec<&str> = call
            .lines()
            .map(str::trim)
            .take_while(|l| !l.starts_with('}'))
            .collect();

        for field in fields {
            let line = call
                .iter()
                .find(|l| l.starts_with(&format!("{field}:")))
                .unwrap_or_else(|| panic!("{} does not pass {field}", seam.init));
            if let Some((_, why)) = seam.hardcoded.iter().find(|(f, _)| *f == field) {
                assert!(
                    !line.contains("p."),
                    "{field} is listed as hardcoded ({why}) but now reads a parameter — \
                         remove the exception: {line}"
                );
                continue;
            }
            let param = seam
                .renamed
                .iter()
                .find(|(f, _)| *f == field)
                .map_or(field, |(_, p)| *p);
            assert!(
                line.contains(&format!("p.{param}")),
                "{} passes {field} from something other than `p.{param}`: {line}",
                seam.init
            );
            checked += 1;
        }
    }
    // The assertion that stops "the parser stopped matching" from masquerading as "everything
    // is fine" — the same load-bearing shape `every_documented_body_is_the_type_its_handler_returns`
    // uses.
    assert!(checked >= 40, "only {checked} dimensions were compared");
}

/// **Every tool name written in this file is a tool that exists** (ADR-085 Inc.2).
///
/// A tool's name is derived from its `async fn`, and then written out again by hand wherever a
/// body records a metric or refuses. There were 179 such sites before this increment and only
/// the `ok_json` ones were checked by anything — `every_typed_tool_result_is_canaried` reads
/// them to find the result type. The other ~120 reach [`record_tool`], which puts the string
/// into a **Prometheus label** (`yagra_mcp_tool_calls_total{tool="…"}`) and a `tracing` field.
/// A typo there mints a series for a tool that does not exist: it compiles, it passes every
/// test, and it is invisible on screen. Nothing was wrong when this was written — all 179 were
/// correct — so this is the guard the next edit needs, not a repair of the last one.
///
/// Two spellings are checked, because the increment deliberately left both:
///   1. `const TOOL` — one per function that talks about a tool (68 of them),
///   2. a literal still passed straight to a helper — the three `bad_*` refusals, used once
///      each in a function named after its tool, where a constant would be noise.
///
/// ⚠️ **What this cannot catch: a function naming a *different real* tool.** It checks the set
/// of names, not the assignment. The wrapper half is exact — see the sibling assertion below —
/// but a `*_in` body has no mechanical link back to its tool, so `list_nodes_in` declaring
/// `get_node_status` would pass here. Said out loud rather than left implied, the way
/// `openapi.rs::every_documented_body_is_the_type_its_handler_returns` states its own
/// partiality: a guard whose blind spot is undocumented gets trusted for what it never did.
#[test]
fn every_tool_name_written_here_is_a_tool_that_exists() {
    // Needles assembled at runtime: this test reads its own file, so a literal one would match
    // itself and the test would describe nothing.
    // `tool_surface()` is already the code half — it cuts each file at its own `#[cfg(test)]`
    // and drops the modules that are test-only in full (ADR-086). The `.split()` that used to
    // be here is gone on purpose: over a concatenation it kept only the *first* file's code,
    // which left this test checking 10 of 36 tools while every assertion below still passed.
    let surface_src = crate::mcp::tool_source::tool_surface();
    let surface: &str = surface_src.as_str();
    let declared = crate::api::route_table::declared_mcp_tools();

    let mut names: Vec<(String, &str)> = Vec::new();
    let const_needle = format!("const {}: &str = {}", "TOOL", '"');
    for chunk in surface.split(&const_needle).skip(1) {
        names.push((
            chunk.chars().take_while(|c| *c != '"').collect(),
            "const TOOL",
        ));
    }
    for helper in [
        "ok_json_value",
        "ok_json",
        "tool_api_error",
        "tool_bad_params",
        "tool_error",
        "tool_unavailable",
        "tool_forbidden",
    ] {
        for name in call_sites_of(surface, helper) {
            names.push((name, "a literal argument"));
        }
    }

    for (name, how) in &names {
        assert!(
            declared.contains(name),
            "`{name}` is written here as {how} but names no tool. It would be recorded as a \
                 Prometheus label and read as a tool that exists; the tools that do exist are: {}",
            declared.iter().cloned().collect::<Vec<_>>().join(", ")
        );
    }

    // The load-bearing half. Every assertion above is vacuously true if the parse stops
    // matching, and a source-text parser stops matching for reasons as small as rustfmt
    // rewrapping a line — so "found nothing" must fail rather than pass (ADR-083's lesson,
    // where mis-pointing a `.split()` went quietly green).
    assert!(
        names.len() >= 65,
        "only found {} tool names in this file; the parser drifted and this test now checks \
             almost nothing",
        names.len()
    );
}

/// …and each `#[tool]` wrapper names **itself** (ADR-085 Inc.2).
///
/// This is the half its sibling cannot do: rmcp derives the published tool name from the
/// `async fn`, so for a wrapper the correct constant is knowable and a mismatch is a real
/// mistake rather than an unverifiable choice. It is also where a copy-pasted wrapper goes
/// wrong — the block below the one you copied keeps the name of the block above.
#[test]
fn every_tool_wrapper_declares_its_own_name() {
    let surface = crate::mcp::tool_source::tool_surface();
    let src: &str = surface.as_str();
    let attr = format!("#[{}(", "tool");
    let const_needle = format!("const {}: &str = {}", "TOOL", '"');
    let mut checked = 0;
    for chunk in src.split(&attr).skip(1) {
        let Some((_, after)) = chunk.split_once("async fn ") else {
            continue;
        };
        let name = after.split('(').next().unwrap_or("?").trim();
        let declared = after
            .split_once(&const_needle)
            .map(|(_, rest)| rest.chars().take_while(|c| *c != '"').collect::<String>())
            .unwrap_or_default();
        assert_eq!(
            declared, name,
            "the `{name}` tool declares its name as {declared:?}; every metric and refusal in \
                 its body would be filed under the wrong tool"
        );
        checked += 1;
    }
    // 🚨 **An independent floor, because the assertion below is not one** (ADR-086). `checked`
    // and `declared_mcp_tools()` are both derived from the same source text, so a surface read
    // in part shrinks both and they go on agreeing — the equality would hold at the wrong
    // number and this test would pass having visited a fraction of the tools. That is the third
    // shape of "quietly green" this repository has met: ADR-083's moved `.split()` target,
    // ADR-085's needle that stopped matching, and now *both sides of a comparison sharing a
    // source*. `declared_mcp_tools()` carries the same floor internally; this one is here so
    // the failure names this parser when it is this parser that drifted.
    assert!(
        checked >= 34,
        "only {checked} `#[tool]` wrappers were visited; the parser drifted or the surface was \
             read in part, and the equality below would agree with it either way"
    );
    assert_eq!(
        checked,
        crate::api::route_table::declared_mcp_tools().len(),
        "the parser did not visit every declared tool"
    );
}

/// The tool names passed as a literal first argument to `fname(` in `src`.
///
/// A near-twin of `dto.rs`'s `call_sites`, and deliberately not shared with it: that one
/// **deduplicates**, because it answers "which tools serialize a type" and a floor over it
/// counts distinct tools. This one must not, because its floor counts *sites* — the thing that
/// tells you the parser is still matching. Unifying them would make one of the two floors mean
/// something other than what its message says.
fn call_sites_of(src: &str, fname: &str) -> Vec<String> {
    let mut out = Vec::new();
    for chunk in src.split(&format!("{fname}(")).skip(1) {
        let rest = chunk.trim_start();
        let Some(inner) = rest.strip_prefix('"') else {
            continue;
        };
        let name: String = inner.chars().take_while(|c| *c != '"').collect();
        if !name.is_empty() {
            out.push(name);
        }
    }
    out
}

// ── The WS-F guard ──────────────────────────────────────────────────────────────────────────

#[test]
fn every_tool_takes_a_request_context() {
    // `/mcp` admits group-scoped principals now, so a tool that does not consult the caller
    // returns the whole fleet — silently, with no compile error and no failing assertion
    // anywhere. A tool body can only reach the caller through its `RequestContext`, so taking
    // one is the observable marker that the question was asked. This does not prove the answer
    // is *used* correctly; it proves nobody added a tool that cannot ask.
    let surface = crate::mcp::tool_source::tool_surface();
    let src: &str = surface.as_str();
    // Assembled at runtime — this test reads its own file, so literal needles would match
    // themselves and pass forever.
    let attr = format!("#[{}(", "tool");
    let ctx_param = format!("{}: RequestContext<RoleServer>", "ctx");
    let mut checked = 0;
    for (idx, _) in src.match_indices(&attr) {
        let rest = &src[idx..];
        // The tool's signature runs from its attribute to the opening brace of its body.
        let body_at = rest
            .find(") -> Result<CallToolResult")
            .unwrap_or(rest.len());
        let signature = &rest[..body_at];
        let name = signature
            .find("async fn ")
            .map(|i| signature[i + 9..].split('(').next().unwrap_or("?"))
            .unwrap_or("?");
        assert!(
            signature.contains(&ctx_param),
            "MCP tool `{name}` does not take a RequestContext, so it cannot resolve the \
                 caller's group scope and will answer fleet-wide to a scoped token"
        );
        checked += 1;
    }
    // The load-bearing half: if the parse stops matching, "everything is fine" must not be the
    // answer. There were 17 tools when this was written and 23 after ADR-042 I1.
    assert!(
        checked >= 34,
        "only matched {checked} tools; parser drifted"
    );
}

/// Every tool whose backing tier is absent answers "unavailable" rather than erroring or, worse,
/// panicking on an `unwrap` of the missing handle.
#[tokio::test]
async fn tools_report_unavailable_when_their_tier_is_off() {
    let m = mcp();

    let flows = m
        .top_flows_in(flow_params(Uuid::new_v4()), &unrestricted())
        .await
        .expect("ok result");
    assert_eq!(json_of(&flows)["available"], serde_json::json!(false));

    let status = m
        .node_status_in(
            NodeIdParams {
                node_id: Uuid::new_v4(),
            },
            &unrestricted(),
        )
        .await
        .expect("ok result");
    assert_eq!(json_of(&status)["available"], serde_json::json!(false));

    let neighbors = m
        .neighbors_in(neighbor_params(Uuid::nil()), &unrestricted())
        .await
        .expect("ok result");
    assert_eq!(json_of(&neighbors)["available"], serde_json::json!(false));

    let groups = m
        .list_node_groups_in(ListNodeGroupsParams::default(), &unrestricted())
        .await
        .expect("ok result");
    assert_eq!(json_of(&groups)["available"], serde_json::json!(false));

    let suppressions = m
        .list_suppressions_in(&unrestricted())
        .await
        .expect("ok result");
    assert_eq!(
        json_of(&suppressions)["available"],
        serde_json::json!(false)
    );
}

/// The published permission names match the permissions the arguments actually demand.
///
/// Shared by the `get_config` and `get_system_health` guards so the two cannot drift into
/// checking different things — the defect this exists for is a description that stopped
/// matching its own table, and a second, subtly weaker copy of the check would be that same
/// defect one level up.
///
/// 🚨 These sentences are handed **verbatim to AI clients**. A wrong permission name is not a
/// comment that rots quietly: a model reads it, tells an operator to grant `manage-config`, the
/// operator grants it, and the call is still refused for lacking `manage-system`.
fn a_description_names_exactly_the_permissions_it_demands(
    tool: &str,
    marker: &str,
    args: &[&str],
    distinct_floor: usize,
) {
    let description = crate::api::route_table::declared_mcp_tool_description(tool)
        .unwrap_or_else(|| panic!("{tool} declares a description"));
    let sentence = description
        .split(marker)
        .nth(1)
        .unwrap_or_else(|| panic!("{tool}'s description explains the permissions"))
        .to_string();

    let demanded: std::collections::BTreeSet<String> = args
        .iter()
        .map(|a| permission_label(crate::mcp::folded::required_permission(tool, a)))
        .collect();
    // A floor, so a broken lookup that returned one permission for everything could not pass by
    // naming only that one.
    assert!(
        demanded.len() >= distinct_floor,
        "only {} distinct permissions found across {tool}'s arguments — the lookup drifted",
        demanded.len()
    );

    let missing: Vec<&String> = demanded
        .iter()
        .filter(|l| !sentence.contains(l.as_str()))
        .collect();
    assert!(
        missing.is_empty(),
        "{tool} demands permissions its description never mentions, so a client cannot tell \
             which arguments its token can read: {missing:?}"
    );

    // The other direction: a name left behind after the code moved. `manage-config` sat in
    // `get_system_health`'s text for two `ManageSystem` sections, which sends an operator to
    // grant the wrong permission just as surely as saying nothing at all would.
    //
    // ⚠️ Substring matching is enough only while no demanded label is a substring of an
    // undemanded one — `view` inside `view-audit` is the pair to watch. Neither sentence names
    // `view-audit` today. If that changes, the fix is to match on word boundaries, not to drop
    // the check.
    let surplus: Vec<String> = Permission::ALL
        .iter()
        .map(|p| permission_label(*p))
        .filter(|l| !demanded.contains(l) && sentence.contains(l.as_str()))
        .collect();
    assert!(
        surplus.is_empty(),
        "{tool}'s description names permissions it never demands, so an operator granting what \
             it says would still be refused: {surplus:?}"
    );
}

/// The `get_system_health` twin of
/// [`the_config_description_names_every_permission_it_can_demand`].
///
/// `get_config` has had that guard since ADR-042 and its description is right. This fold — the
/// older and larger of the two — had no guard, and named `manage-config` for two `ManageSystem`
/// sections (`forwarding`, `upgrade`) until 2026-08-21. The two defects are the same one a
/// level apart: [`every_health_section_has_a_folded_row_and_vice_versa`] pins that a row
/// **exists**, this pins that the published text **says what the row says**.
#[test]
fn the_health_description_names_every_permission_it_can_demand() {
    let args: Vec<&str> = HealthSection::NAMES
        .iter()
        .map(|n| HealthSection::parse(n).expect("every NAME parses").arg())
        .collect();
    a_description_names_exactly_the_permissions_it_demands(
        "get_system_health",
        "Sections require different permissions:",
        &args,
        // view / manage-system / manage-credentials.
        3,
    );
}

/// A typo is refused, not silently resolved to a default. `get_system_health` has no sensible
/// default section — every one answers a different question.
/// A refusal must name the permission the way the descriptions do. The stored key is
/// `manage_config`; every description on this surface says `manage-config`, and a model told
/// one thing then shown another has to guess which is real.
#[test]
fn a_permission_is_named_the_way_the_descriptions_name_it() {
    assert_eq!(permission_label(Permission::ManageConfig), "manage-config");
    assert_eq!(permission_label(Permission::AckAlerts), "ack-alerts");
    assert_eq!(permission_label(Permission::ViewAudit), "view-audit");
    assert_eq!(permission_label(Permission::View), "view");
    // The spelling the existing hand-written refusals use, so the two families agree.
    assert!(
        crate::mcp::tool_source::tool_surface().contains("lacks ack-alerts permission"),
        "the older refusals spell it hyphenated; this helper must match them"
    );
}
