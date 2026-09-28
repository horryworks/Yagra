// SPDX-License-Identifier: AGPL-3.0-only
//! The two poller allow-lists, held to each other (ADR-184). Test-only.
//!
//! A poller is granted its subjects in one of two places, and **each covers exactly the deployments
//! the other does not**: the JWT this crate mints (Auth Callout on) and the static `poller` account
//! in `docker/nats/nats-server.conf` (callout off). A subject missing from either is a silent
//! runtime denial on half the deployments, and testing one shape proves nothing about the other
//! (`extensibility.md` §6). This compares them.
//!
//! What is allowed to differ is breadth, not coverage: the static account cannot know a poller's
//! id, so it grants `yagra.poller.assign.>` where the JWT grants `yagra.poller.assign.{id}`. So the
//! check is by NATS matching, in both directions — every subject the JWT grants is covered by a
//! static pattern, and every static pattern covers something the JWT grants.

use crate::{allow_list, PollerScope};

/// Whether the NATS subject pattern `pattern` covers `subject` (itself possibly a pattern, when
/// both sides grant the same wildcard). `*` is one token, `>` is the rest.
fn covers(pattern: &str, subject: &str) -> bool {
    if pattern == subject {
        return true;
    }
    let (p, s): (Vec<&str>, Vec<&str>) =
        (pattern.split('.').collect(), subject.split('.').collect());
    for (i, tok) in p.iter().enumerate() {
        match *tok {
            ">" => return s.len() > i,
            "*" => {
                if s.get(i).is_none_or(|t| *t == ">") {
                    return false;
                }
            }
            lit => {
                if s.get(i) != Some(&lit) {
                    return false;
                }
            }
        }
    }
    p.len() == s.len()
}

/// The quoted subjects in `section { allow [ … ] }` of the `user: poller` block, comments dropped.
fn static_grants(conf: &str, section: &str) -> Vec<String> {
    let block = conf
        .split("user: poller")
        .nth(1)
        .expect("docker/nats/nats-server.conf has a `user: poller` block");
    let body = block
        .split(&format!("{section} {{"))
        .nth(1)
        .unwrap_or_else(|| panic!("the poller block has no `{section}` section"));
    let list = body
        .split('[')
        .nth(1)
        .and_then(|rest| rest.split(']').next())
        .unwrap_or_else(|| panic!("the poller's `{section}` has no allow list"));
    list.lines()
        .map(|l| l.split('#').next().unwrap_or_default().trim())
        .filter_map(|l| l.strip_prefix('"').and_then(|l| l.strip_suffix('"')))
        .map(str::to_owned)
        .collect()
}

#[test]
fn the_static_poller_account_grants_what_the_callout_grants() {
    let conf = std::fs::read_to_string(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../docker/nats/nats-server.conf"),
    )
    .expect("docker/nats/nats-server.conf is readable");
    let minted = allow_list(&PollerScope::new("site-a-1", "site-a"));
    for (section, jwt, floor) in [
        ("publish", &minted.publish, 8),
        ("subscribe", &minted.subscribe, 8),
    ] {
        let fixed = static_grants(&conf, section);
        assert!(
            fixed.len() >= floor,
            "only {} {section} grants read from the static account",
            fixed.len()
        );
        let uncovered: Vec<&String> = jwt
            .iter()
            .filter(|s| !fixed.iter().any(|p| covers(p, s)))
            .collect();
        assert!(
            uncovered.is_empty(),
            "the Auth Callout JWT may {section} {uncovered:?}, but the static `poller` account in \
             docker/nats/nats-server.conf does not — every deployment with callout off is denied"
        );
        let unused: Vec<&String> = fixed
            .iter()
            .filter(|p| !jwt.iter().any(|s| covers(p, s)))
            .collect();
        assert!(
            unused.is_empty(),
            "the static `poller` account may {section} {unused:?}, which the Auth Callout JWT never \
             grants — either the JWT is missing it or the static account is wider than it should be"
        );
    }
}

#[test]
fn a_wildcard_covers_what_nats_would_deliver_and_nothing_else() {
    assert!(covers(
        "yagra.poller.assign.>",
        "yagra.poller.assign.site-a-1"
    ));
    assert!(!covers("yagra.poller.assign.>", "yagra.poller.assign"));
    assert!(covers("yagra.jobs.*", "yagra.jobs.site-a"));
    assert!(!covers("yagra.jobs.*", "yagra.jobs.site-a.x"));
    assert!(!covers("yagra.discovery.jobs.>", "yagra.discovery.cancel"));
    assert!(covers("_INBOX.>", "_INBOX.>"));
    assert!(!covers(
        "yagra.poller.assign.site-a-1",
        "yagra.poller.assign.>"
    ));
}
