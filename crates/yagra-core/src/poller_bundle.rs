// SPDX-License-Identifier: AGPL-3.0-only
//! The archive a remote site is handed to stand a poller up (ADR-065 Increment 4).
//!
//! ## What this replaces
//!
//! Standing up a site used to mean collecting four things by hand from three screens and a shell:
//! the composition (from a git checkout), the bus URL (typed, with the shared password pasted into
//! it), the CA certificate (produced by an `openssl` invocation on the central host and copied out
//! of a bind mount), and a `.env` assembled from a dialog that only generated *some* of it. Every
//! one of those is a place to get a character wrong, and the failure they produce is identical and
//! silent: a poller that starts, logs a connection error at a site nobody is watching, and never
//! appears centrally.
//!
//! So the whole set is built here, once, at the moment the token exists — because that is the only
//! moment it exists. The token is displayed nowhere and stored nowhere: only its digest is kept
//! (`migrations/0090_poller_token.sql`), so if this archive is lost the answer is to issue a new
//! token, not to look the old one up.
//!
//! ## Why a tar.gz rather than a zip
//!
//! `tar` and `flate2` are already dependencies (the support bundle, ADR-045) and the recipient is a
//! Linux host running Docker by definition. Adding a zip writer to hand a tarball to a machine that
//! has `tar` would be a second archive format for one caller.
//!
//! ## Deliberately not included
//!
//! The image itself. A site pulls a published release from GHCR, and an air-gapped site loads it
//! the way `docker-compose.poller.yml` documents — putting a gigabyte in a browser download to save
//! one command would make this unusable at the size it is most needed.

use std::io::Write;

/// Everything the archive is built from. Assembled by the API handler; kept as a struct so this
/// module needs no `ApiState` and can be tested with three strings.
pub struct SiteBundleInput<'a> {
    /// The poller's id — also the NATS connection username, which is what the Auth Callout scopes on.
    pub poller_id: &'a str,
    /// The pool it serves. Nodes are assigned to pollers by pool (ADR-009).
    pub pool: &'a str,
    /// Host or IP the site dials. Must be a subject alternative name on `ca_certificate`, or the
    /// TLS handshake fails at the site with nothing visible centrally.
    pub host: &'a str,
    /// Bus port on that host.
    pub port: u16,
    /// The token, in the clear. The only copy that will ever exist.
    pub token: &'a str,
    /// The bus certificate to pin, in PEM.
    pub ca_certificate: &'a str,
    /// `docker-compose.poller.yml`, read out of this image so the site runs the composition that
    /// shipped with the core it will talk to.
    pub compose: &'a str,
    /// Whether this site should upgrade itself when the deployment does (ADR-051 Inc.4).
    ///
    /// Written as a `COMPOSE_PROFILES` line, which is what decides whether the composition's
    /// `yagra-poller-updater` service exists at all. It goes in the `.env` and **not** in the
    /// composition because an apply reinstalls the composition from the target image (ADR-050
    /// decision 5) and leaves the `.env` alone — so this is the only file where the site's answer
    /// survives its own upgrades.
    pub self_upgrade: bool,
    /// Core's version, for the README.
    pub core_version: &'a str,
    /// Archive mtime, Unix seconds. Injected — `tar` needs one and a test must not depend on now.
    pub mtime: u64,
}

/// Where the site puts the certificate. `docker-compose.poller.yml` mounts `./certs` at
/// `/etc/yagra/certs` and defaults `YAGRA_BUS_CA_FILE` to `/etc/yagra/certs/server-cert.pem`, so
/// this path is what makes the `.env` able to stay silent about it.
const CERT_PATH: &str = "certs/server-cert.pem";

/// Build the archive. Returns gzipped tar bytes.
///
/// # Errors
/// Only io errors from the in-memory writers, which cannot occur in practice — propagated rather
/// than unwrapped so a future change to the sink does not turn into a panic on a request path.
pub fn build(input: &SiteBundleInput<'_>) -> std::io::Result<Vec<u8>> {
    let env = render_env(input);
    let readme = render_readme(input);

    let mut tar = tar::Builder::new(flate2::write::GzEncoder::new(
        Vec::new(),
        flate2::Compression::default(),
    ));
    append(
        &mut tar,
        "README.txt",
        readme.as_bytes(),
        input.mtime,
        0o644,
    )?;
    append(
        &mut tar,
        "docker-compose.poller.yml",
        input.compose.as_bytes(),
        input.mtime,
        0o644,
    )?;
    // 0600: it carries the token. The mode is inside the archive, so it survives the copy to the
    // site — which is the only place it can be set, since nobody will remember to.
    append(&mut tar, ".env", env.as_bytes(), input.mtime, 0o600)?;
    append(
        &mut tar,
        CERT_PATH,
        input.ca_certificate.as_bytes(),
        input.mtime,
        0o644,
    )?;
    tar.into_inner()?.finish()
}

/// The `.env` that sits beside `docker-compose.poller.yml`.
///
/// Only the values the composition *requires*, plus the one switch that has nowhere else to live —
/// every other setting has a working default in the compose file, and copying those defaults here
/// would create a second place they are written down, in a file no upgrade ever replaces.
///
/// That last clause is exactly why `COMPOSE_PROFILES` belongs here and not there: being the file no
/// upgrade replaces is a *liability* for a default and the whole point for a switch.
fn render_env(i: &SiteBundleInput<'_>) -> String {
    format!(
        "# Yagra remote poller — generated for {id} on core {ver}.\n\
         # Keep this file private: YAGRA_BUS_URL contains this poller's bus token.\n\
         YAGRA_POLLER_ID={id}\n\
         YAGRA_POLLER_POOL={pool}\n\
         # The username is the poller id: the central Auth Callout scopes this connection's\n\
         # permissions on it, so it may not be changed without the token becoming invalid.\n\
         YAGRA_BUS_URL=tls://{id}:{token}@{host}:{port}\n\
         {upgrade}",
        id = i.poller_id,
        pool = i.pool,
        token = i.token,
        host = i.host,
        port = i.port,
        ver = i.core_version,
        upgrade = env_self_upgrade(i.self_upgrade),
    )
}

/// The `COMPOSE_PROFILES` stanza, or nothing at all.
///
/// **Off writes no line rather than an empty one.** An empty `COMPOSE_PROFILES=` would work, but it
/// would also silently override a value the site's operator had set for some other purpose, and it
/// reads as "Yagra has an opinion here" when the opinion is "not ours to hold". Absent means the
/// composition's own default applies, which is off.
fn env_self_upgrade(on: bool) -> String {
    if !on {
        return String::new();
    }
    "# Runs the `yagra-poller-updater` sidecar, so a single press of Settings > Upgrade at the\n\
     # central deployment replaces this poller too (ADR-051). That sidecar holds this host's\n\
     # Docker socket; empty the value below to switch it off. Switch it off HERE and not in the\n\
     # composition — an upgrade reinstalls that file and would undo the edit.\n\
     COMPOSE_PROFILES=self-upgrade\n"
        .to_owned()
}

fn render_readme(i: &SiteBundleInput<'_>) -> String {
    format!(
        "Yagra remote poller — {id}\n\
         =====================================\n\
         \n\
         Generated by Yagra core {ver} for pool `{pool}`, connecting to {host}:{port}.\n\
         \n\
         Put this whole directory on the machine at the site, then:\n\
         \n\
           docker compose -f docker-compose.poller.yml up -d\n\
         \n\
         Within about ten seconds it should appear at Settings > Pollers on the central\n\
         deployment, marked online.\n\
         \n\
         Files\n\
         -----\n\
           .env                        this poller's identity and its bus token\n\
           certs/server-cert.pem       the bus certificate this poller pins\n\
           docker-compose.poller.yml   the composition, taken from core {ver}\n\
         \n\
         The token\n\
         ---------\n\
         The token in .env exists only in this archive. Yagra stored only a digest of it, so\n\
         it cannot be shown again. If you lose this file, issue a new token at\n\
         Settings > Pollers — the old one stops working the moment you do.\n\
         \n\
         {upgrade}\
         If it does not connect\n\
         ----------------------\n\
         `docker compose -f docker-compose.poller.yml logs poller` is the first place to look;\n\
         the poller also writes rotated log files inside the container under /var/log/yagra.\n\
         The two things that go wrong most often:\n\
         \n\
           * The address in YAGRA_BUS_URL is not one the certificate covers. It must be\n\
             exactly `{host}` — not another name for the same machine, and not its IP if the\n\
             certificate names a hostname. Reissue the certificate centrally to add a name.\n\
           * The bus port is not reachable from here. Check {host}:{port} with\n\
             `nc -z {host} {port}` before assuming the credentials are wrong: a firewall and a\n\
             bad token look identical from this end.\n\
         \n\
         Ports this poller listens on\n\
         ----------------------------\n\
         With the defaults it binds syslog on 1514/udp, SNMP traps on 1162/udp and NetFlow on\n\
         2055/udp, on the host's own interfaces. Point this site's devices at those, or\n\
         redirect the standard low ports to them on the host firewall.\n",
        id = i.poller_id,
        pool = i.pool,
        host = i.host,
        port = i.port,
        ver = i.core_version,
        upgrade = readme_self_upgrade(i.self_upgrade),
    )
}

/// The README's account of the self-upgrade sidecar, or nothing at all.
///
/// Written for the person standing up the site, who did not choose this and may be the one who has
/// to answer for it — so it says what runs, what that grants, and how to refuse, in that order. The
/// "off" case says nothing rather than reassuring: a paragraph explaining an absent container is a
/// paragraph nobody needs, and every line here competes with the two failure modes below it that
/// people actually hit.
fn readme_self_upgrade(on: bool) -> String {
    if !on {
        return String::new();
    }
    "Upgrading with the central deployment\n\
     ------------------------------------\n\
     COMPOSE_PROFILES=self-upgrade in .env runs a second container here, yagra-poller-updater.\n\
     When someone presses Settings > Upgrade centrally, core sends this poller the release tag\n\
     over the bus, the poller writes it into a shared volume, and that container installs it.\n\
     The poller itself never runs anything; the updater is the only thing here holding the\n\
     Docker socket, which on any host means it can start a privileged container.\n\
     \n\
     Bounds worth knowing: the updater listens on no network, a command carries a version tag\n\
     and never a repository (so the worst a forged one installs is a published Yagra poller\n\
     release), and the bus permits this poller to receive upgrade commands but never to send\n\
     one, so no site can order an upgrade at another. Whoever controls the central deployment\n\
     can, by design, replace the Yagra poller on this host.\n\
     \n\
     To refuse it, empty the value:\n\
     \n\
       COMPOSE_PROFILES=\n\
       docker compose -p yagra-poller -f docker-compose.poller.yml up -d --remove-orphans\n\
     \n\
     Do that in .env, not by editing docker-compose.poller.yml. An upgrade reinstalls the\n\
     composition from the release being installed, so an edit there lasts until the next one;\n\
     .env is never replaced. With it off, this poller stays on whatever version you last\n\
     pulled, and the central Settings > Upgrade page lists it by name as left behind.\n\
     \n"
    .to_owned()
}

fn append<W: Write>(
    tar: &mut tar::Builder<W>,
    path: &str,
    bytes: &[u8],
    mtime: u64,
    mode: u32,
) -> std::io::Result<()> {
    let mut header = tar::Header::new_gnu();
    header.set_size(bytes.len() as u64);
    header.set_mode(mode);
    header.set_mtime(mtime);
    tar.append_data(&mut header, path, bytes)
}

/// A filename for the download. Kept beside the builder so the two agree on the extension.
#[must_use]
pub fn file_name(poller_id: &str) -> String {
    format!("yagra-poller-{poller_id}.tar.gz")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn input<'a>(token: &'a str, host: &'a str) -> SiteBundleInput<'a> {
        SiteBundleInput {
            poller_id: "edge-tokyo-1",
            pool: "tokyo",
            host,
            port: 4222,
            token,
            ca_certificate: "-----BEGIN CERTIFICATE-----\nMIIB\n-----END CERTIFICATE-----\n",
            compose: "name: yagra-poller\nservices:\n  poller: {}\n",
            self_upgrade: true,
            core_version: "0.2.12",
            mtime: 1_786_444_236,
        }
    }

    fn entries(bytes: &[u8]) -> Vec<(String, String, u32)> {
        let mut archive = tar::Archive::new(flate2::read::GzDecoder::new(bytes));
        archive
            .entries()
            .expect("entries")
            .map(|e| {
                let mut e = e.expect("entry");
                let path = e.path().expect("path").display().to_string();
                let mode = e.header().mode().expect("mode");
                let mut body = String::new();
                std::io::Read::read_to_string(&mut e, &mut body).expect("body");
                (path, body, mode)
            })
            .collect()
    }

    #[test]
    fn the_archive_carries_exactly_what_a_site_needs_to_come_up() {
        let out = build(&input("T0kenT0kenT0ken", "yagra.example.net")).expect("build");
        let names: Vec<String> = entries(&out).into_iter().map(|(n, _, _)| n).collect();
        for want in [
            "README.txt",
            "docker-compose.poller.yml",
            ".env",
            "certs/server-cert.pem",
        ] {
            assert!(
                names.iter().any(|n| n == want),
                "missing {want} in {names:?}"
            );
        }
        assert_eq!(
            names.len(),
            4,
            "an unexpected file joined the bundle: {names:?}"
        );
    }

    #[test]
    fn the_env_holds_the_token_and_the_cert_does_not() {
        // The one arrangement that must never invert: the certificate is handed around freely and
        // the `.env` is the secret. A token that leaked into the certificate file would be copied
        // into every ticket where somebody pastes "the CA cert".
        let out = build(&input("T0kenT0kenT0ken", "yagra.example.net")).expect("build");
        for (name, body, mode) in entries(&out) {
            match name.as_str() {
                ".env" => {
                    assert!(body.contains("T0kenT0kenT0ken"), "{body}");
                    assert!(
                        body.contains("YAGRA_BUS_URL=tls://edge-tokyo-1:T0kenT0kenT0ken@yagra.example.net:4222"),
                        "{body}"
                    );
                    // The username is the poller id, not the shared `poller` account: the callout
                    // scopes the connection's permissions on it.
                    assert!(!body.contains("tls://poller:"), "{body}");
                    assert_eq!(mode & 0o777, 0o600, ".env must not be world-readable");
                }
                "certs/server-cert.pem" | "README.txt" | "docker-compose.poller.yml" => {
                    assert!(
                        !body.contains("T0kenT0kenT0ken"),
                        "the token leaked into {name}"
                    );
                }
                other => panic!("unexpected entry {other}"),
            }
        }
    }

    #[test]
    fn the_readme_names_the_two_failures_that_look_identical_from_the_site() {
        // A poller that cannot connect gives the same symptom for a wrong SAN and a blocked port,
        // and the site engineer cannot see the central end. Naming both is the whole value of
        // shipping a README rather than a bare `.env`.
        let out = build(&input("t", "yagra.example.net")).expect("build");
        let readme = entries(&out)
            .into_iter()
            .find(|(n, _, _)| n == "README.txt")
            .expect("README")
            .1;
        assert!(readme.contains("yagra.example.net"), "{readme}");
        assert!(readme.contains("nc -z"), "{readme}");
        assert!(readme.contains("Settings > Pollers"), "{readme}");
        // It must also say the token cannot be recovered, or somebody will delete the archive.
        assert!(readme.contains("cannot be shown again"), "{readme}");
    }

    #[test]
    fn the_compose_file_travels_verbatim() {
        // The site must run the composition that shipped with the core it talks to (ADR-050
        // decision 5's reasoning, applied one hop out). Re-rendering it here would be a second copy.
        let compose = "name: yagra-poller\nservices:\n  poller:\n    image: x\n";
        let mut i = input("t", "h");
        i.compose = compose;
        let out = build(&i).expect("build");
        let got = entries(&out)
            .into_iter()
            .find(|(n, _, _)| n == "docker-compose.poller.yml")
            .expect("compose")
            .1;
        assert_eq!(got, compose);
    }

    #[test]
    fn the_file_name_says_which_poller_it_is_for() {
        assert_eq!(file_name("edge-1"), "yagra-poller-edge-1.tar.gz");
    }

    /// 🚨 The `.env` may only name the poller id as the bus username while the shipped bus can
    /// actually validate one.
    ///
    /// This is bug 10 of ADR-065 Inc.6, and it was not a coding mistake — it was two artefacts
    /// disagreeing with nobody to notice. [`render_env`] writes
    /// `YAGRA_BUS_URL=tls://<id>:<token>@…` and says, in the file it writes, that "the central Auth
    /// Callout scopes this connection's permissions on it". The only thing that can validate such a
    /// name is that callout — and the shipped `nats-server.conf` had it commented out, with an
    /// instruction to uncomment it that `bus-cert-init` erases on the next `up`. Every site bundle
    /// ever issued declared a precondition nothing created, and the failure landed at the remote
    /// end: `authentication error - User "yagra-poller2a"`, at a site nobody was watching.
    ///
    /// So the two are pinned to each other here. Comment the include out and this fails with the
    /// consequence rather than with a diff.
    #[test]
    fn the_env_names_the_poller_id_only_because_the_shipped_bus_can_validate_one() {
        let env = render_env(&input("T0kenT0kenT0ken", "yagra.example.net"));
        assert!(
            env.contains("YAGRA_BUS_URL=tls://edge-tokyo-1:"),
            "the bundle stopped naming the poller id; if that was deliberate, this pairing is what \
             needs deciding again: {env}"
        );
        let conf = std::fs::read_to_string("../../docker/nats/nats-server.conf")
            .expect("the NATS configuration ships in the core image");
        let live: Vec<&str> = conf
            .lines()
            .map(str::trim)
            .filter(|l| !l.starts_with('#'))
            .collect();
        assert!(
            live.iter()
                .any(|l| *l == format!("include \"{}\"", crate::bus_callout::CONF_FILE)),
            "the bundle's `.env` presents the poller id as the bus username, and the shipped \
             nats-server.conf has no live path to an `auth_callout` block that could validate it. \
             Every site issued a bundle from this build would be refused at connect time, centrally \
             invisible. Either restore the include, or stop writing the id as the username."
        );
        // The static `poller` account must NOT be what a site is pointed at: it is one shared
        // password for the whole fleet, which is the tenant boundary this feature exists to draw.
        assert!(!env.contains("tls://poller:"), "{env}");
    }

    /// The switch lands in the `.env`, and only there (ADR-051 Inc.4 decision 15).
    ///
    /// 🚨 **The accepting case comes first and carries the weight.** A `render_env` that never
    /// wrote the line at all would satisfy every "must not contain" below while making the whole
    /// increment inert — the failure mode this repository has already paid for
    /// (`rejection-only-tests-pass-when-everything-rejects`). So the first assertion is that the
    /// line is *present*, spelled exactly as the composition's `profiles:` entry.
    #[test]
    fn the_self_upgrade_switch_is_an_env_line_that_appears_and_disappears() {
        let mut i = input("T0kenT0kenT0ken", "yagra.example.net");

        i.self_upgrade = true;
        let on = render_env(&i);
        assert!(
            on.contains("COMPOSE_PROFILES=self-upgrade\n"),
            "an enabled site gets no profile line, so its updater is never created: {on}"
        );

        i.self_upgrade = false;
        let off = render_env(&i);
        assert!(
            !off.contains("COMPOSE_PROFILES"),
            "a site that declined still gets the line: {off}"
        );
        // Not an empty assignment either — that would override a value the site set for something
        // else of its own, which is not this bundle's business.
        assert!(!off.contains("COMPOSE_PROFILES="), "{off}");

        // Whichever way it goes, the identity half is untouched: the switch must not be able to
        // cost a site its connection.
        for env in [&on, &off] {
            assert!(env.contains("YAGRA_POLLER_ID=edge-tokyo-1\n"), "{env}");
            assert!(env.contains("YAGRA_BUS_URL=tls://edge-tokyo-1:"), "{env}");
        }
    }

    /// The profile name here and the one in the shipped composition are the same string.
    ///
    /// They are two files with no compiler between them, and the failure is silent in the worst
    /// way: compose accepts an unknown profile name without complaint, creates nothing, and the
    /// site comes up looking exactly like a healthy one that declined. Core would then list it as
    /// left behind forever while its `.env` says it opted in.
    #[test]
    fn the_env_names_the_profile_the_shipped_composition_actually_declares() {
        let compose = std::fs::read_to_string("../../docker-compose.poller.yml")
            .expect("the remote-site composition ships with the product");
        let env = render_env(&input("T0kenT0kenT0ken", "yagra.example.net"));
        let name = env
            .lines()
            .find_map(|l| l.strip_prefix("COMPOSE_PROFILES="))
            .expect("the bundle writes a profile line by default");
        assert!(
            compose
                .lines()
                .map(str::trim)
                .any(|l| l == format!("profiles: [\"{name}\"]")),
            "the bundle enables the profile {name:?}, which no service in \
             docker-compose.poller.yml declares — compose would create nothing and say nothing"
        );
        // And the service that carries it is the updater, not something incidental.
        assert!(
            compose.contains("yagra-poller-updater:"),
            "the composition no longer defines the site updater, so this check is reading a file \
             that can no longer honour the switch the bundle writes"
        );
    }

    /// A site is told what it is being asked to run, in the archive that asks it.
    ///
    /// The person unpacking this did not choose it and may be the one who has to justify it, so
    /// the README states the socket plainly rather than only the convenience. The `off` direction
    /// is asserted too: an absent container needs no paragraph, and every line here competes with
    /// the two connection failures people actually hit.
    #[test]
    fn the_readme_names_the_docker_socket_when_the_sidecar_runs_and_stays_quiet_when_it_does_not() {
        let mut i = input("T0kenT0kenT0ken", "yagra.example.net");

        i.self_upgrade = true;
        let on = render_readme(&i);
        assert!(on.contains("yagra-poller-updater"), "{on}");
        assert!(
            on.contains("Docker socket"),
            "the README sells the convenience without naming what it grants: {on}"
        );
        assert!(
            on.contains("COMPOSE_PROFILES=\n"),
            "the README must show how to refuse, not only how to accept: {on}"
        );

        i.self_upgrade = false;
        let off = render_readme(&i);
        assert!(!off.contains("yagra-poller-updater"), "{off}");
        // The parts every site needs survive either way — the section is additive, not a fork.
        for r in [&on, &off] {
            assert!(r.contains("If it does not connect"), "{r}");
            assert!(r.contains("Ports this poller listens on"), "{r}");
        }
    }
}
