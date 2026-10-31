// SPDX-License-Identifier: AGPL-3.0-only
//! oxidized's configuration file, written by Yagra (ADR-210 stage 2 decision 14).
//!
//! oxidized reads its configuration once, when it starts, so the file is written by a one-shot
//! container (`yagra-core oxidized-config`) that oxidized waits for. Nothing in it is tunable by an
//! operator yet; what changes per deployment is the source token.
//!
//! The token is what `GET /internal/oxidized/nodes` demands. That route returns device logins in
//! plaintext. It is served only on core's internal listener, which the composition never publishes
//! (ADR-217), so the header is what stands between another container on the compose network and
//! every login Yagra holds for config backup.

use std::path::Path;

/// The source token file, in the shared directory. Read by core, never by oxidized (which gets the
/// token inside the configuration file).
pub(crate) const TOKEN_FILE: &str = "source-token";
/// oxidized's configuration file name inside `OXIDIZED_HOME`.
pub(crate) const CONFIG_FILE: &str = "config";
/// The header oxidized presents the token in.
pub(crate) const TOKEN_HEADER: &str = "x-yagra-oxidized-token";

/// Where oxidized reads its node list, and where its hook reports how each fetch ended. Core's
/// internal listener on the compose network, which the composition never publishes (ADR-217) —
/// these routes return device logins and the Git credential, and the northbound port refuses them.
pub(crate) const SOURCE_URL: &str = "http://core:8090/internal/oxidized/nodes";
pub(crate) const EVENTS_URL: &str = "http://core:8090/internal/oxidized/events";
/// Where the push script asks whether to push and reports how the push went (ADR-210 stage 4).
pub(crate) const PUSH_URL: &str = "http://core:8090/internal/oxidized/push";
/// Where the save script asks what to do after a fetch and reports what it did (ADR-222).
pub(crate) const SAVE_URL: &str = "http://core:8090/internal/oxidized/save";

/// The push script's file name in the shared directory, and the script itself (stage 4 decision 28).
/// Written beside the configuration file, so the same one-shot owns both and an upgrade replaces
/// both.
pub(crate) const PUSH_SCRIPT_FILE: &str = "yagra-push.rb";
const PUSH_SCRIPT: &str = include_str!("yagra-push.rb");
/// The save script (ADR-222): the one script that sends a device anything but a fetch, and only
/// what core hands it from `save::SAVE_COMMANDS`.
pub(crate) const SAVE_SCRIPT_FILE: &str = "yagra-save.rb";
pub(crate) const SAVE_SCRIPT: &str = include_str!("yagra-save.rb");
/// Where oxidized sees the shared directory (`docker-compose.deploy.yml` mounts it there).
const SHARED_DIR_IN_OXIDIZED: &str = "/var/lib/yagra/oxidized";

/// The revision of [`render`]'s output, also written as `YAGRA_OXIDIZED_CONFIG_REVISION` on the
/// `oxidized` service in `docker-compose.deploy.yml`.
///
/// 🚨 oxidized reads its configuration only when it starts, and an upgrade that does not change the
/// oxidized image does not restart it — the one-shot rewrites the file and the old configuration
/// keeps running. A changed environment variable is what makes compose recreate the container, so
/// the revision moves whenever the rendered text does. `the_revision_moves_with_the_configuration`
/// fails until both are bumped.
pub(crate) const CONFIG_REVISION: u32 = 9;

const TOKEN_LEN: usize = 40;

/// A fresh token: letters and digits, starting with a letter.
///
/// It is written into YAML unquoted-safe form either way, but a value of digits alone is what YAML
/// reads as a number — the bus password learned that the hard way (`api/bus.rs::random_secret`).
fn random_token() -> String {
    use rand::Rng;
    const LETTERS: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz";
    const ALPHABET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789";
    let mut rng = rand::thread_rng();
    std::iter::once(LETTERS[rng.gen_range(0..LETTERS.len())] as char)
        .chain((1..TOKEN_LEN).map(|_| ALPHABET[rng.gen_range(0..ALPHABET.len())] as char))
        .collect()
}

/// The token in `home`, created on first use. Kept across restarts, so a running oxidized and a
/// restarted core still agree.
pub(crate) fn ensure_token(home: &Path) -> std::io::Result<String> {
    if let Ok(existing) = read_token(home) {
        return Ok(existing);
    }
    let token = random_token();
    yagra_base::atomic_file::write_atomically(&home.join(TOKEN_FILE), token.as_bytes(), 0o600)?;
    Ok(token)
}

/// The token in `home`. An empty or missing file is an error: core then refuses every caller.
pub(crate) fn read_token(home: &Path) -> std::io::Result<String> {
    let token = std::fs::read_to_string(home.join(TOKEN_FILE))?;
    let token = token.trim();
    if token.is_empty() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "the oxidized source token file is empty",
        ));
    }
    Ok(token.to_owned())
}

/// oxidized's configuration file for this deployment.
///
/// - `retries: 0` makes one fetch one login (stage 3): with a retry, a wrong password is sent twice
///   for every attempt the lockout limits count once.
/// - The `yagra` hook reports how each fetch ended to `POST /internal/oxidized/events`, with the
///   source token, the node and oxidized's error type — never its error message, which can carry
///   what the device printed. `async`, so oxidized does not wait on core. `curl` is named by its
///   path, which is how it was measured to run from a hook.
///
/// - `remove_secret` strips passwords and keys from what is stored, model by model — it is not
///   exhaustive, which is why the text stays behind `ManageConfig` and off MCP.
/// - `interval: 0` means oxidized fetches nothing on its own (stage 6 decision 58): every fetch is
///   one Yagra asked for with `/node/next`, which `next_adds_job` turns into an immediate job —
///   the scheduled run, the rotation, or a person's "fetch now". oxidized 0.37's worker stops at a
///   node that was not asked for when the interval is zero (measured in `worker.rb`); with a day,
///   it fetched every node again after each restart, since it does not remember its fetches.
/// - `input.default` is SSH alone. Every node is handed its own `input`, so this applies only to a
///   node the source left without one — and telnet, which carries the login in cleartext, is never
///   reached by falling back from a blocked port 22 unless a person chose it for that node.
/// - ⚠️ `ssh.secure: false` accepts any host key. oxidized offers only "always verify" or "never",
///   with no trust on first use, so `true` would refuse every first connection until someone fills
///   its known_hosts by hand. Pinning keys is ADR-210's remaining work, not done here.
/// - The REST API listens on every interface of the container, because core reaches it across the
///   compose network; the container publishes no port.
/// - `group` comes from the source, and `single_repo` keeps every node's file in one Git repository.
/// - The `yagra-push` hook runs the push script after every fetch that succeeded (stage 4): the
///   script asks core at [`PUSH_URL`] whether the repository's head needs pushing, so the hook
///   costs one local request when there is no destination. Its limit is longer than the script's
///   own (two and a half minutes waiting for another push, two for its own).
/// - The `yagra-save` hook runs the save script after every fetch that succeeded (ADR-222): it
///   asks core at the save URL what to do, which for almost every node is nothing, and exits
///   before loading oxidized when it is. Its limit covers a save and two comparisons.
///   🚨 `OXIDIZED_HOME` is set on the command line because oxidized starts a hook with
///   `unsetenv_others`: without it the script loads oxidized's defaults instead of this file, and
///   every save fails (measured on a lab deployment, 2026-10-31).
/// - The pid file and crash reports stay in oxidized's own home (its data volume), not in the shared
///   directory: the image's start script removes a stale pid file only at that path, so one left
///   anywhere else by a crash would stop the next start.
#[must_use]
pub(crate) fn render(
    token: &str,
    source_url: &str,
    events_url: &str,
    push_url: &str,
    save_url: &str,
) -> String {
    format!(
        "---
# Written by Yagra (yagra-core oxidized-config). Edits here are replaced on the next start.
username: unused
password: unused
model: ios
resolve_dns: false
interval: 0
threads: 8
timeout: 30
timelimit: 300
retries: 0
next_adds_job: true
debug: false
pid: /home/oxidized/.config/oxidized/pid
crash:
  directory: /home/oxidized/.config/oxidized/crashes
  hostnames: false
vars:
  remove_secret: true
extensions:
  oxidized-web:
    load: true
    listen: 0.0.0.0
    port: 8888
input:
  default: ssh
  debug: false
  ssh:
    secure: false
output:
  default: git
  git:
    user: Yagra
    email: oxidized@yagra.invalid
    repo: /home/oxidized/.config/oxidized/configs.git
    single_repo: true
source:
  default: http
  http:
    url: {source_url}
    read_timeout: 60
    headers:
      {TOKEN_HEADER}: \"{token}\"
    map:
      name: name
      ip: ip
      model: model
      input: input
      group: group
      username: username
      password: password
    vars_map:
      enable: enable
      ssh_port: ssh_port
      telnet_port: telnet_port
hooks:
  yagra:
    type: exec
    events: [node_success, node_fail]
    async: true
    timeout: 30
    cmd: '/usr/bin/curl -fsS -m 20 -o /dev/null -X POST -H \"{TOKEN_HEADER}: {token}\" --data-urlencode \"event=$OX_EVENT\" --data-urlencode \"node=$OX_NODE_NAME\" --data-urlencode \"group=$OX_NODE_GROUP\" --data-urlencode \"ip=$OX_NODE_IP\" --data-urlencode \"err_type=$OX_ERR_TYPE\" {events_url}'
  yagra-push:
    type: exec
    events: [node_success]
    async: true
    timeout: 300
    cmd: 'YAGRA_TOKEN=\"{token}\" YAGRA_PUSH_URL=\"{push_url}\" /usr/bin/ruby {SHARED_DIR_IN_OXIDIZED}/{PUSH_SCRIPT_FILE}'
  yagra-save:
    type: exec
    events: [node_success]
    async: true
    timeout: 300
    cmd: 'OXIDIZED_HOME=\"{SHARED_DIR_IN_OXIDIZED}\" YAGRA_TOKEN=\"{token}\" YAGRA_SAVE_URL=\"{save_url}\" /usr/bin/ruby {SHARED_DIR_IN_OXIDIZED}/{SAVE_SCRIPT_FILE}'
"
    )
}

/// Write the token (if new) and the configuration file into `home`. Leaves an identical file alone.
pub(crate) fn write(
    home: &Path,
    source_url: &str,
    events_url: &str,
    push_url: &str,
    save_url: &str,
) -> anyhow::Result<()> {
    let token = ensure_token(home)?;
    for (file, text) in [
        (PUSH_SCRIPT_FILE, PUSH_SCRIPT),
        (SAVE_SCRIPT_FILE, SAVE_SCRIPT),
    ] {
        let script = home.join(file);
        if std::fs::read_to_string(&script).ok().as_deref() != Some(text) {
            yagra_base::atomic_file::write_atomically(&script, text.as_bytes(), 0o640)?;
        }
    }
    let body = render(&token, source_url, events_url, push_url, save_url);
    let path = home.join(CONFIG_FILE);
    if std::fs::read_to_string(&path).ok().as_deref() == Some(body.as_str()) {
        return Ok(());
    }
    // 0640: the directory is setgid to oxidized's group, so the group bit is what lets it read.
    yagra_base::atomic_file::write_atomically(&path, body.as_bytes(), 0o640)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_configuration_strips_secrets_and_carries_the_token_and_source() {
        let body = render("Abc123", SOURCE_URL, EVENTS_URL, PUSH_URL, SAVE_URL);
        assert!(body.contains("remove_secret: true"));
        assert!(body.contains("x-yagra-oxidized-token: \"Abc123\""));
        assert!(body.contains("url: http://core:8090/internal/oxidized/nodes"));
        assert!(body.contains("pid: /home/oxidized/.config/oxidized/pid"));
        // The session log would hold the password a device was sent.
        assert!(body.contains("  debug: false"));
        // One fetch, one login.
        assert!(body.contains("retries: 0"));
        // The hook reports with the token, and never the error message.
        assert!(body.contains("x-yagra-oxidized-token: Abc123"));
        assert!(body.contains("err_type=$OX_ERR_TYPE"));
        assert!(!body.contains("OX_ERR_REASON"));
        assert!(body.contains(EVENTS_URL));
        // The push hook runs the script from the shared directory, with the token and core's URL.
        assert!(body.contains(
            "cmd: 'YAGRA_TOKEN=\"Abc123\" YAGRA_PUSH_URL=\"http://core:8090/internal/oxidized/push\" \
             /usr/bin/ruby /var/lib/yagra/oxidized/yagra-push.rb'"
        ));
        // The save hook runs after a successful fetch only (ADR-222 decision 7).
        assert!(body.contains(
            "cmd: 'OXIDIZED_HOME=\"/var/lib/yagra/oxidized\" YAGRA_TOKEN=\"Abc123\" \
             YAGRA_SAVE_URL=\"http://core:8090/internal/oxidized/save\" \
             /usr/bin/ruby /var/lib/yagra/oxidized/yagra-save.rb'"
        ));
    }

    /// The push never speaks HTTP/2 (ADR-215 increment 2, decision 4): the libcurl in the pinned
    /// oxidized image has a use-after-free on an HTTP/2 upload that the server resets, and that is
    /// the one finding in the image Yagra can reach. Remove with the image that fixes it.
    #[test]
    fn the_push_is_pinned_to_http_1_1() {
        let push = PUSH_SCRIPT
            .lines()
            .skip_while(|l| !l.contains("Open3.capture2e(env, 'timeout'"))
            .take(3)
            .collect::<String>();
        assert!(push.contains("'-c', 'http.version=HTTP/1.1'"), "{push}");
        assert!(push.contains("'push'"), "{push}");
    }

    /// The script carries no rule of its own: it reports only what core's patterns name, never
    /// git's output, and it never forces a push.
    #[test]
    fn the_push_script_reports_a_kind_and_never_the_output() {
        assert!(PUSH_SCRIPT.contains("job['patterns']"));
        assert!(PUSH_SCRIPT.contains("'failure' => failure.to_s"));
        assert!(!PUSH_SCRIPT.contains("'output'"));
        assert!(!PUSH_SCRIPT.contains("--force"));
        assert!(!PUSH_SCRIPT.contains("+HEAD"));
        // The token goes through the environment, never git's arguments.
        assert!(PUSH_SCRIPT.contains("GIT_CONFIG_VALUE_0"));
        assert!(PUSH_SCRIPT.contains(&format!("'{TOKEN_HEADER}'")));
        // 🚨 The script reads the repository the configuration names and keeps only the files under
        // the folder Yagra's nodes are filed in. Either one out of step makes every push an empty
        // tree, which deletes every device's file at the destination.
        assert!(PUSH_SCRIPT.contains(&format!("GROUP = '{}'", super::super::GROUP)));
        let repo = render("T", "S", "E", "P", "V")
            .lines()
            .find_map(|l| l.trim().strip_prefix("repo: "))
            .expect("the configuration names a repository")
            .to_owned();
        assert!(PUSH_SCRIPT.contains(&format!("REPO = '{repo}'")));
    }

    /// The rendered text is pinned by its digest, and the digest by [`CONFIG_REVISION`] and the
    /// composition's copy of it: change one and this names the other two.
    #[test]
    fn the_revision_moves_with_the_configuration() {
        use sha2::Digest;
        // The push script is not in the digest: oxidized runs it afresh on every hook, so a changed
        // script needs no restart, only the one-shot that rewrites it on every start. The three URLs
        // are: they are what oxidized dials, and moving them to the internal listener (ADR-217) was
        // a change to the running configuration this digest could not see while it rendered
        // placeholders in their place.
        let digest = sha2::Sha256::digest(
            render("T", SOURCE_URL, EVENTS_URL, PUSH_URL, SAVE_URL).as_bytes(),
        );
        let hex: String = digest.iter().map(|b| format!("{b:02x}")).collect();
        let pinned: &[(u32, &str)] = &[(
            CONFIG_REVISION,
            "fcbb7783d72add6210d0b5c6bbbffcfffa7178728828f2c1b40bd7c4df4a83f4",
        )];
        assert_eq!(
            pinned,
            &[(CONFIG_REVISION, hex.as_str())],
            "render() changed: bump CONFIG_REVISION and YAGRA_OXIDIZED_CONFIG_REVISION in \
             docker-compose.deploy.yml, then pin the new digest here"
        );
        let compose = include_str!("../../../../docker-compose.deploy.yml");
        let line = format!("YAGRA_OXIDIZED_CONFIG_REVISION: \"{CONFIG_REVISION}\"");
        assert!(
            compose.contains(&line),
            "docker-compose.deploy.yml must carry {line} on the oxidized service"
        );
    }

    #[test]
    fn a_token_starts_with_a_letter_and_is_letters_and_digits() {
        for _ in 0..200 {
            let t = random_token();
            assert_eq!(t.len(), TOKEN_LEN);
            assert!(t.as_bytes()[0].is_ascii_alphabetic(), "{t}");
            assert!(t.bytes().all(|b| b.is_ascii_alphanumeric()), "{t}");
        }
    }

    #[test]
    fn the_token_is_created_once_and_kept() {
        let dir = std::env::temp_dir().join(format!("yagra-ox-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).expect("dir");
        let first = ensure_token(&dir).expect("create");
        assert_eq!(ensure_token(&dir).expect("reread"), first);
        write(&dir, SOURCE_URL, EVENTS_URL, PUSH_URL, SAVE_URL).expect("write");
        let cfg = std::fs::read_to_string(dir.join(CONFIG_FILE)).expect("config");
        assert!(cfg.contains(&first));
        let script = std::fs::read_to_string(dir.join(PUSH_SCRIPT_FILE)).expect("script");
        assert_eq!(script, PUSH_SCRIPT);
        let save = std::fs::read_to_string(dir.join(SAVE_SCRIPT_FILE)).expect("save script");
        assert_eq!(save, SAVE_SCRIPT);
        std::fs::remove_dir_all(&dir).ok();
    }
}
