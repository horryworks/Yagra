// SPDX-License-Identifier: AGPL-3.0-only
//! What this poller does when core tells it to replace itself (ADR-051).
//!
//! **Nothing here executes anything**, and that is the whole design: this process has the network
//! connection, so it is the one that must not be able to run a container. It validates the command,
//! writes a request file into the hand-off directory, and goes back to waiting; a sidecar with the
//! Docker socket is what acts on it. ADR-050 drew the same line centrally.
//!
//! ⚠️ **The three validators here are a deliberate second copy** of core's
//! (`yagra-core/src/upgrade.rs`). Duplication is normally the thing to avoid, but not across a trust
//! boundary — the point is that the value is checked by *both* sides, so neither has to assume the
//! other did. Each copy owes its own tests, and until ADR-103 this side had none while its doc said
//! otherwise; they are at the bottom of this file now.
//!
//! The capability claim that tells core a site updater is really there is **not** here: it is a
//! heartbeat field, and it lives with the heartbeat (`heartbeat.rs::self_upgrade_cap`). Its name
//! says "upgrade" and its only caller is the beat, which is why it went by the caller.

use std::path::PathBuf;
use std::sync::Arc;

use yagra_bus::NatsBus;
use yagra_telemetry::{spawn_cancellable, CancellationToken};

use crate::PollerIdentity;

/// Subscribe to this poller's upgrade commands and run the hand-off loop.
///
/// **Subscribed only when the hand-off directory is configured.** With nowhere to write, receiving
/// the command would be a log line and a lie — core's page would then show the site as
/// "will upgrade" when nothing can act on it. The same condition gates the capability claim, and
/// deliberately reads the same env var rather than sharing a cached answer: the two are asking
/// different questions (is there a directory / is a sidecar alive in it).
pub(crate) async fn start(
    bus: &Arc<NatsBus>,
    identity: &PollerIdentity,
    shutdown: &CancellationToken,
) -> anyhow::Result<()> {
    if let Some(dir) = crate::env_nonempty("YAGRA_UPGRADE_DIR") {
        let sub = Box::pin(bus.subscribe_poller_upgrades(&identity.id).await?);
        spawn_cancellable(
            shutdown,
            run_upgrade_loop(sub, identity.id.clone(), PathBuf::from(dir)),
        );
    }
    Ok(())
}

/// Turn upgrade commands into hand-off files for the site updater to act on (ADR-051).
///
/// **This loop executes nothing.** It validates, writes a file, and goes back to waiting — the same
/// division ADR-050 drew centrally, where core writes a request and a container with the Docker
/// socket reads it. The poller is the piece with a network connection, so it is the piece that must
/// not be able to run anything.
async fn run_upgrade_loop<S>(mut stream: S, poller_id: String, dir: PathBuf)
where
    S: futures::Stream<Item = yagra_bus::PollerUpgradeMsg> + Unpin,
{
    use futures::StreamExt;
    while let Some(msg) = stream.next().await {
        // Addressed to someone else: the subject already routed it, so this can only be a mistake
        // (or a probe). Drop it rather than act on a routing error — the site whose id is on the
        // message is not this one, and installing its release here would be silently wrong.
        if msg.poller_id != poller_id {
            tracing::warn!(intended = %msg.poller_id, "ignoring an upgrade command addressed elsewhere");
            continue;
        }
        // Validate here as well as in the updater. Neither check is redundant: this one keeps a
        // malformed value out of the shared volume at all, and the updater's keeps it out of the
        // `docker` invocation even if something else wrote the file.
        if !is_release_tag(&msg.tag) {
            tracing::warn!(tag = %msg.tag, "refusing an upgrade command with an invalid release tag");
            continue;
        }
        if !is_run_id(&msg.run_id) {
            tracing::warn!("refusing an upgrade command with an invalid run id");
            continue;
        }
        let command = match msg.step {
            yagra_bus::UpgradeStep::Prefetch => "prefetch",
            yagra_bus::UpgradeStep::Apply => "apply",
        };
        let body = format!(
            "schema=1\nid={}\ncommand={}\ntag={}\nrequested_by={}\nrequested_at={}\n",
            msg.run_id,
            command,
            msg.tag,
            sanitize_actor(&msg.requested_by),
            msg.requested_at,
        );
        // Temp-then-rename, so the updater never reads a partially written request (ADR-050).
        let tmp = dir.join("request.tmp");
        let write =
            std::fs::write(&tmp, body).and_then(|()| std::fs::rename(&tmp, dir.join("request")));
        match write {
            Ok(()) => {
                tracing::info!(tag = %msg.tag, %command, run = %msg.run_id, "handed an upgrade request to the site updater")
            }
            Err(e) => tracing::error!(error = %e, "failed to write the upgrade request"),
        }
    }
}

/// Is this a release tag this poller will pass on? Mirrors core's `upgrade::is_valid_tag` — `v`
/// plus a three-part semver with an optional short suffix, and nothing else.
///
/// A second copy of a rule is normally the thing to avoid, but not across a trust boundary: the
/// point is that the value is checked by *both* sides, so neither has to assume the other did. The
/// rule is small and stable enough to state twice, and each side's copy has a test.
///
/// ⚠️ **That last sentence was false on this side until ADR-103.** Core's copy really is tested
/// (`upgrade.rs::a_run_id_cannot_name_a_path`); this one had nothing, while the doc said otherwise
/// — on the three functions standing between a value core sent and a filename this process writes.
/// The tests are at the bottom of this file now.
fn is_release_tag(tag: &str) -> bool {
    let Some(rest) = tag.strip_prefix('v') else {
        return false;
    };
    if rest.is_empty() || rest.len() > 40 {
        return false;
    }
    let (core, suffix) = match rest.split_once('-') {
        Some((c, s)) => (c, Some(s)),
        None => (rest, None),
    };
    let parts: Vec<&str> = core.split('.').collect();
    if parts.len() != 3 {
        return false;
    }
    if !parts
        .iter()
        .all(|p| !p.is_empty() && p.len() <= 6 && p.bytes().all(|b| b.is_ascii_digit()))
    {
        return false;
    }
    match suffix {
        None => true,
        Some(s) => !s.is_empty() && s.len() <= 16 && s.bytes().all(|b| b.is_ascii_alphanumeric()),
    }
}

/// A run id is a UUID in hyphenated form, and nothing else — it becomes part of a filename.
fn is_run_id(id: &str) -> bool {
    id.len() == 36 && id.bytes().all(|b| b.is_ascii_hexdigit() || b == b'-')
}

/// Reduce an actor to characters that cannot break out of the `key=value` line the updater parses.
fn sanitize_actor(who: &str) -> String {
    let clean: String = who
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '@' | '-'))
        .take(64)
        .collect();
    if clean.is_empty() {
        "unknown".to_owned()
    } else {
        clean
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The tag becomes an image reference on the other side of the hand-off file.
    ///
    /// 🚨 **The accepting cases come first, and are not a formality**: a validator that refuses
    /// everything satisfies every rejection below while stopping the poller from ever upgrading
    /// (`rejection-only-tests-pass-when-everything-rejects`).
    #[test]
    fn a_release_tag_is_a_v_and_three_numbers() {
        for good in [
            "v0.2.17",
            "v1.0.0",
            "v1.0.0-beta1",
            "v10.20.30-rc2",
            "v0.0.1",
        ] {
            assert!(
                is_release_tag(good),
                "{good:?} is a tag this poller must pass on"
            );
        }
        for bad in [
            "",
            "0.2.17",   // core's own check spells the `v`, so this side must too
            "v1.2",     // two parts
            "v1.2.3.4", // four
            "v1.2.",
            "v1.2.3-",    // an empty suffix is not a suffix
            "v1.2.3-a b", // a space would split the updater's `key=value` line
            "v1.2.3-../..",
            "v1.2.3456789", // a part longer than six digits
            "latest",
        ] {
            assert!(
                !is_release_tag(bad),
                "{bad:?} must not reach the site updater"
            );
        }
    }

    /// The run id becomes part of a filename, so it may be a UUID and nothing else.
    ///
    /// The same property core asserts on its own copy — that is the point of there being two.
    #[test]
    fn a_run_id_cannot_name_a_path() {
        assert!(is_run_id("0f8fad5b-d9cb-469f-a165-70867728950e"));
        for bad in [
            "",
            "../../etc/passwd",
            "0f8fad5b-d9cb-469f-a165-70867728950e/x",
            "0f8fad5b-d9cb-469f-a165-7086772895",    // too short
            "0f8fad5b-d9cb-469f-a165-70867728950ee", // too long
            "0f8fad5b-d9cb-469f-a165-708677289.0e",  // a dot would split the extension
            "0f8fad5b d9cb 469f a165 70867728950e",
        ] {
            assert!(!is_run_id(bad), "{bad:?} must not become a file name");
        }
    }

    /// The actor is written into a `key=value` line the updater parses back.
    #[test]
    fn an_actor_cannot_break_out_of_its_line() {
        assert_eq!(sanitize_actor("admin@example.com"), "admin@example.com");
        assert_eq!(sanitize_actor("ops-team_1.2"), "ops-team_1.2");
        // A newline would end the line early and everything after it would read as a new key.
        assert_eq!(sanitize_actor("admin\ncommand=apply"), "admincommandapply");
        assert_eq!(sanitize_actor("a=b"), "ab");
        // ⚠️ Dots survive, and are meant to: this guards the *line format*, not a path. The value
        // is written after `requested_by=` and never used to open anything.
        assert_eq!(sanitize_actor("../../root"), "....root");
        // Nothing left is not an empty actor, it is an unknown one — the field is never blank.
        assert_eq!(sanitize_actor(""), "unknown");
        assert_eq!(sanitize_actor("\n\n"), "unknown");
        assert_eq!(sanitize_actor(&"x".repeat(200)).len(), 64);
    }
}
