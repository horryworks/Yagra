// SPDX-License-Identifier: AGPL-3.0-only
//! oxidized's configuration file, written by Yagra (ADR-210 stage 2 decision 14).
//!
//! oxidized reads its configuration once, when it starts, so the file is written by a one-shot
//! container (`yagra-core oxidized-config`) that oxidized waits for. Nothing in it is tunable by an
//! operator yet; what changes per deployment is the source token.
//!
//! The token is what `GET /internal/oxidized/nodes` demands. That route returns device logins in
//! plaintext and is on core's own port, which is published on the LAN, so the header is the only
//! thing between a neighbour on the network and every login Yagra holds for config backup.

use std::path::Path;

/// The source token file, in the shared directory. Read by core, never by oxidized (which gets the
/// token inside the configuration file).
pub(crate) const TOKEN_FILE: &str = "source-token";
/// oxidized's configuration file name inside `OXIDIZED_HOME`.
pub(crate) const CONFIG_FILE: &str = "config";
/// The header oxidized presents the token in.
pub(crate) const TOKEN_HEADER: &str = "x-yagra-oxidized-token";

/// Where oxidized reads its node list, and where its hook reports how each fetch ended. Core's own
/// port on the compose network.
pub(crate) const SOURCE_URL: &str = "http://core:8080/internal/oxidized/nodes";
pub(crate) const EVENTS_URL: &str = "http://core:8080/internal/oxidized/events";

/// The revision of [`render`]'s output, also written as `YAGRA_OXIDIZED_CONFIG_REVISION` on the
/// `oxidized` service in `docker-compose.deploy.yml`.
///
/// 🚨 oxidized reads its configuration only when it starts, and an upgrade that does not change the
/// oxidized image does not restart it — the one-shot rewrites the file and the old configuration
/// keeps running. A changed environment variable is what makes compose recreate the container, so
/// the revision moves whenever the rendered text does. `the_revision_moves_with_the_configuration`
/// fails until both are bumped.
pub(crate) const CONFIG_REVISION: u32 = 3;

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
/// - `interval` is a day; Yagra asks for an earlier fetch with `/node/next`, which
///   `next_adds_job` turns into an immediate job.
/// - `input.default` is SSH alone. Every node is handed its own `input`, so this applies only to a
///   node the source left without one — and telnet, which carries the login in cleartext, is never
///   reached by falling back from a blocked port 22 unless a person chose it for that node.
/// - ⚠️ `ssh.secure: false` accepts any host key. oxidized offers only "always verify" or "never",
///   with no trust on first use, so `true` would refuse every first connection until someone fills
///   its known_hosts by hand. Pinning keys is ADR-210's remaining work, not done here.
/// - The REST API listens on every interface of the container, because core reaches it across the
///   compose network; the container publishes no port.
/// - `group` comes from the source, and `single_repo` keeps every node's file in one Git repository.
/// - The pid file and crash reports stay in oxidized's own home (its data volume), not in the shared
///   directory: the image's start script removes a stale pid file only at that path, so one left
///   anywhere else by a crash would stop the next start.
#[must_use]
pub(crate) fn render(token: &str, source_url: &str, events_url: &str) -> String {
    format!(
        "---
# Written by Yagra (yagra-core oxidized-config). Edits here are replaced on the next start.
username: unused
password: unused
model: ios
resolve_dns: false
interval: 86400
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
hooks:
  yagra:
    type: exec
    events: [node_success, node_fail]
    async: true
    timeout: 30
    cmd: '/usr/bin/curl -fsS -m 20 -o /dev/null -X POST -H \"{TOKEN_HEADER}: {token}\" --data-urlencode \"event=$OX_EVENT\" --data-urlencode \"node=$OX_NODE_NAME\" --data-urlencode \"group=$OX_NODE_GROUP\" --data-urlencode \"ip=$OX_NODE_IP\" --data-urlencode \"err_type=$OX_ERR_TYPE\" {events_url}'
"
    )
}

/// Write the token (if new) and the configuration file into `home`. Leaves an identical file alone.
pub(crate) fn write(home: &Path, source_url: &str, events_url: &str) -> anyhow::Result<()> {
    let token = ensure_token(home)?;
    let body = render(&token, source_url, events_url);
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
        let body = render("Abc123", SOURCE_URL, EVENTS_URL);
        assert!(body.contains("remove_secret: true"));
        assert!(body.contains("x-yagra-oxidized-token: \"Abc123\""));
        assert!(body.contains("url: http://core:8080/internal/oxidized/nodes"));
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
    }

    /// The rendered text is pinned by its digest, and the digest by [`CONFIG_REVISION`] and the
    /// composition's copy of it: change one and this names the other two.
    #[test]
    fn the_revision_moves_with_the_configuration() {
        use sha2::Digest;
        let digest = sha2::Sha256::digest(render("T", "S", "E").as_bytes());
        let hex: String = digest.iter().map(|b| format!("{b:02x}")).collect();
        let pinned: &[(u32, &str)] = &[(
            CONFIG_REVISION,
            "54990e2fa30c666b9f68fd799f0e5fa34d7d4ca5b2cac067ba215f1b7b791e00",
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
        write(&dir, SOURCE_URL, EVENTS_URL).expect("write");
        let cfg = std::fs::read_to_string(dir.join(CONFIG_FILE)).expect("config");
        assert!(cfg.contains(&first));
        std::fs::remove_dir_all(&dir).ok();
    }
}
