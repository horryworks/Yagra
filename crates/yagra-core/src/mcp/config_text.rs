// SPDX-License-Identifier: AGPL-3.0-only
//! A device configuration made fit to hand an AI client (ADR-210 stage 10).
//!
//! oxidized's `remove_secret` already ran over every text this module sees. It works model by
//! model and can miss a line, which is why ADR-210 decision 16 kept configuration text off `/mcp`
//! in the first place. Stage 10 reopens it behind three guards, and this file is the second one
//! (decision 84): Yagra blanks secrets again, by its own rules, on the way out of `/mcp` only.
//!
//! ⚠️ **This is a second net, not a guarantee.** It knows the shapes in [`CASES`] and the words in
//! [`SECRET_WORDS`]; a vendor spelling neither covers still leaks. The guards that make stage 10
//! safe to switch on are the deployment switch (decision 85) and the audit row per read
//! (decision 86). Say so wherever this is described.
//!
//! The rule is deliberately coarse. A line that names a secret word loses **everything after the
//! word**, not just the value, because the value's position differs per vendor and per command
//! (`password 7 <v>`, `secret 9 <v>`, `community <v> RO`, `cipher <v>`) and guessing it wrong is a
//! leak, while blanking a harmless `RO` costs a model one fact. A word with nothing after it
//! (`service password-encryption`) leaves the line unchanged.
//!
//! Everything here is pure, so the whole rule is tested without oxidized.

use regex::{Regex, RegexBuilder};

/// What a blanked value reads as.
pub(crate) const REDACTED: &str = "<redacted>";

/// A token naming a secret: the token itself, or one of its `-`/`_` parts, is one of these
/// (`pre-shared-key` → `key`, `encrypted-password` → `password`). Whole parts only, so
/// `keepalive`, `authentication` and `community-list`'s neighbours stay readable... except
/// `community-list` itself, which loses its tail: over-blanking is the safe direction.
const SECRET_WORDS: &[&str] = &[
    "password",
    "passwd",
    "secret",
    "community",
    "key",
    "cipher",
    "psk",
    "passphrase",
    "auth",
    "priv",
];

/// The longest `match` pattern accepted. A model writes it; a long one is a mistake or a probe.
pub(crate) const MATCH_MAX_CHARS: usize = 200;

/// Lines a page carries when the caller names no `limit` (decision 87).
pub(crate) const DEFAULT_LIMIT: usize = 500;

/// The most lines one page may carry, whatever the caller asks.
pub(crate) const MAX_LIMIT: usize = 2_000;

/// Pieces that mark a secret anywhere inside a token, for the vendors that run words together
/// (FortiGate's `psksecret`, a `wpapassword`). Only pieces that do not occur inside ordinary
/// configuration words: `pass` alone would take `passive-interface`, `key` would take `keepalive`.
const SECRET_PIECES: &[&str] = &["secret", "passw", "psk"];

/// Whether `token` names a secret.
fn names_secret(token: &str) -> bool {
    let t = token
        .trim_matches(|c: char| !c.is_alphanumeric())
        .to_ascii_lowercase();
    !t.is_empty()
        && (SECRET_WORDS.contains(&t.as_str())
            || t.split(['-', '_']).any(|part| SECRET_WORDS.contains(&part))
            || SECRET_PIECES.iter().any(|p| t.contains(p)))
}

/// Whether the token at `i` begins a value that names no secret word before it:
/// FortiGate's `ENC <value>`, and Cisco's `snmp-server host <address> … <community>`, whose
/// community string trails the line with no word in front of it.
fn opens_unnamed_secret(line: &str, spans: &[(usize, usize)], i: usize) -> bool {
    let token = &line[spans[i].0..spans[i].1];
    if token == "ENC" {
        return true;
    }
    i == 1
        && token.eq_ignore_ascii_case("host")
        && line[spans[0].0..spans[0].1].eq_ignore_ascii_case("snmp-server")
}

/// `scheme://user:password@host…` with the user and password blanked, or `None` for a token
/// carrying no credentials in a URL.
///
/// The delimiter is the **last** `@` after `://`, not the last one before the first `/`: a
/// password may hold a `/` itself, and cutting at it would leave the password standing. A path
/// that happens to hold an `@` loses its host as well, which is the safe direction.
fn url_userinfo(token: &str) -> Option<String> {
    let scheme_end = token.find("://")? + 3;
    let rest = &token[scheme_end..];
    let at = rest.rfind('@')?;
    Some(format!("{}{REDACTED}{}", &token[..scheme_end], &rest[at..]))
}

/// Whether a URL's query names a secret in any parameter (`?key=…`, `&community=…`), which
/// [`names_secret`] does not see because `key` is a whole word only.
fn query_names_secret(token: &str) -> bool {
    token.find('?').is_some_and(|q| {
        token[q + 1..]
            .split(['&', ';'])
            .any(|kv| names_secret(kv.split('=').next().unwrap_or_default()))
    })
}

/// A token blanked on its own shape, whatever word precedes it: a stored hash whole, a URL's
/// credentials.
fn clean_token(token: &str) -> Option<String> {
    if looks_encrypted(token) {
        Some(REDACTED.to_owned())
    } else {
        url_userinfo(token)
    }
}

/// `line[..upto]` with every token of `spans` (all inside it) put through [`clean_token`], and
/// whether any was changed. Used for what a secret word leaves standing before it, as well as
/// for a whole line with no secret word, so nothing kept is kept unexamined.
fn clean_range(line: &str, spans: &[(usize, usize)], upto: usize) -> (String, bool) {
    let mut out = String::with_capacity(upto);
    let mut last = 0;
    let mut changed = false;
    for &(s, e) in spans {
        if let Some(r) = clean_token(&line[s..e]) {
            out.push_str(&line[last..s]);
            out.push_str(&r);
            last = e;
            changed = true;
        }
    }
    out.push_str(&line[last..upto]);
    (out, changed)
}

/// Whether `token` is shaped like a stored hash or an encrypted secret, whatever word precedes it:
/// crypt (`$1$`, `$5$`, `$6$`), Cisco type 8/9 (`$8$`, `$9$`), Comware (`$h$`, `$c$`), Huawei's
/// wrapped cipher text (`%^%#…`, `%$%$…`, `@%@%…`).
fn looks_encrypted(token: &str) -> bool {
    let t = token.trim_matches(|c: char| c == '"' || c == '\'' || c == ';');
    if ["%^%#", "%$%$", "@%@%", "%@%@"]
        .iter()
        .any(|p| t.starts_with(p))
    {
        return true;
    }
    let Some(rest) = t.strip_prefix('$') else {
        return false;
    };
    match rest.find('$') {
        Some(n) => (1..=3).contains(&n) && rest[..n].bytes().all(|b| b.is_ascii_alphanumeric()),
        None => false,
    }
}

/// The byte spans of the whitespace-separated tokens of `line`.
fn token_spans(line: &str) -> Vec<(usize, usize)> {
    let mut out = Vec::new();
    let mut start = None;
    for (i, c) in line.char_indices() {
        match (c.is_whitespace(), start) {
            (true, Some(s)) => {
                out.push((s, i));
                start = None;
            }
            (false, None) => start = Some(i),
            _ => {}
        }
    }
    if let Some(s) = start {
        out.push((s, line.len()));
    }
    out
}

/// One line with its secrets blanked, and whether anything was blanked.
pub(crate) fn redact_line(line: &str) -> (String, bool) {
    let spans = token_spans(line);
    for (i, &(s, e)) in spans.iter().enumerate() {
        let token = &line[s..e];
        let scheme_at = token.find("://");
        // `password=hunter2`, `key:abc`, `key=ftp://…` — the word and its value in one token.
        // Checked before the URL rule, so a value shaped like a URL is still a value; the one
        // separator skipped is a URL's own `scheme:`.
        if let Some(cut) = token.find(['=', ':']) {
            if scheme_at != Some(cut)
                && names_secret(&token[..cut])
                && (cut + 1 < token.len() || i + 1 < spans.len())
            {
                let (kept, _) = clean_range(line, &spans[..i], s);
                return (format!("{kept}{}{REDACTED}", &token[..=cut]), true);
            }
        }
        // A URL naming a secret (`ftp://u:secretpw@h`, `…?key=x`) is a value, not a word to
        // keep: it goes whole, with the rest of the line.
        if scheme_at.is_some() {
            if names_secret(token) || query_names_secret(token) {
                let (kept, _) = clean_range(line, &spans[..i], s);
                return (format!("{kept}{REDACTED}"), true);
            }
            continue;
        }
        if names_secret(token) || opens_unnamed_secret(line, &spans, i) {
            if i + 1 < spans.len() {
                let (kept, _) = clean_range(line, &spans[..=i], e);
                return (format!("{kept} {REDACTED}"), true);
            }
            // The word ends the line: nothing follows it to blank.
            continue;
        }
    }
    // No secret word: blank any token shaped like a stored hash, alone, and the credentials of a
    // URL that carries them.
    clean_range(line, &spans, line.len())
}

/// A text's lines with their secrets blanked, in order, each with whether it was blanked.
///
/// A PEM private key spans lines whose body carries no word at all, so the lines between a
/// `-----BEGIN … PRIVATE KEY-----` and its `-----END` are blanked whole.
///
/// The two marker lines are not trusted to hold only the marker: a key written on one line
/// (`-----BEGIN … KEY----- MIIE… -----END … KEY-----`) or a value that starts on the marker's own
/// line (`private-key "-----BEGIN RSA PRIVATE KEY-----MIIE…`) would otherwise pass through. So
/// a marker line keeps only the marker itself; anything else on it is blanked.
pub(crate) fn redact_lines<'a>(lines: impl Iterator<Item = &'a str>) -> Vec<(String, bool)> {
    let mut in_key = false;
    let mut out = Vec::new();
    for raw in lines {
        // `str::lines` hands this lines without their ending; a diff's slices keep it.
        let line = raw.trim_end_matches(['\r', '\n']);
        if in_key {
            match line.find("-----END") {
                Some(at) => {
                    in_key = false;
                    out.push(end_marker_line(line, at));
                }
                None => out.push((REDACTED.to_owned(), true)),
            }
            continue;
        }
        if let Some(at) = private_key_begin(line) {
            let (text, ends_here) = begin_marker_line(line, at);
            in_key = !ends_here;
            out.push((text, true));
            continue;
        }
        out.push(redact_line(line));
    }
    out
}

/// Where a PEM private-key header starts on `line`, if it holds one.
fn private_key_begin(line: &str) -> Option<usize> {
    let at = line.find("-----BEGIN")?;
    line[at..].contains("PRIVATE KEY").then_some(at)
}

/// The end of a `-----BEGIN …-----` / `-----END …-----` marker that starts at `at`: just past its
/// closing dashes, or the end of the line when it has none.
fn marker_end(line: &str, at: usize) -> usize {
    let after_open = at + 5;
    line[after_open..]
        .find("-----")
        .map_or(line.len(), |n| after_open + n + 5)
}

/// A private-key header line: the marker kept, everything before it put through [`redact_line`]
/// (`private-key "` loses its tail there anyway), and anything after it blanked. Returns whether
/// the key also ends on this line, in which case the whole line is blanked.
fn begin_marker_line(line: &str, at: usize) -> (String, bool) {
    let end = marker_end(line, at);
    if line[end..].contains("-----END") {
        return (REDACTED.to_owned(), true);
    }
    let lead = line[..at].trim_end();
    let marker = &line[at..end];
    let tail = line[end..].trim_matches(|c: char| c.is_whitespace() || c == '"' || c == ';');
    let mut text = if lead.is_empty() {
        String::new()
    } else {
        format!("{} ", redact_line(lead).0)
    };
    text.push_str(marker);
    if !tail.is_empty() {
        text.push(' ');
        text.push_str(REDACTED);
    }
    (text, false)
}

/// A private-key footer line: the marker kept, anything before it on the line (the key's last
/// characters, when it was not wrapped) blanked.
fn end_marker_line(line: &str, at: usize) -> (String, bool) {
    let before = line[..at].trim_matches(|c: char| c.is_whitespace() || c == '"');
    let end = marker_end(line, at);
    let marker = &line[at..end];
    if before.is_empty() {
        (line[..end].to_owned(), false)
    } else {
        (format!("{REDACTED} {marker}"), true)
    }
}

/// Compile a caller's `match` pattern, or say why not.
pub(crate) fn compile_match(pattern: &str) -> Result<Regex, String> {
    if pattern.chars().count() > MATCH_MAX_CHARS {
        return Err(format!("match may be at most {MATCH_MAX_CHARS} characters"));
    }
    RegexBuilder::new(pattern)
        .size_limit(1 << 20)
        .dfa_size_limit(1 << 20)
        .build()
        .map_err(|_| "match is not a valid regular expression".to_owned())
}

/// One page of numbered lines: which ones `pattern` keeps, from `offset`, at most `limit`.
pub(crate) struct Page<T> {
    /// The lines the pattern kept, before paging.
    pub(crate) matched: usize,
    pub(crate) lines: Vec<T>,
    /// More matched lines follow this page.
    pub(crate) truncated: bool,
}

/// Page `items`, keeping those whose text `pattern` matches. The text is read through `text_of`,
/// so this runs over already-blanked lines and a pattern can never find a value by matching it.
pub(crate) fn page<T>(
    items: Vec<T>,
    text_of: impl Fn(&T) -> &str,
    pattern: Option<&Regex>,
    offset: usize,
    limit: usize,
) -> Page<T> {
    let kept: Vec<T> = items
        .into_iter()
        .filter(|item| pattern.is_none_or(|p| p.is_match(text_of(item))))
        .collect();
    let matched = kept.len();
    let limit = limit.clamp(1, MAX_LIMIT);
    let lines: Vec<T> = kept.into_iter().skip(offset).take(limit).collect();
    let truncated = offset.saturating_add(lines.len()) < matched;
    Page {
        matched,
        lines,
        truncated,
    }
}

/// Of a diff's line kinds (`true` = changed), which to keep: every changed line and up to
/// `context` lines either side of one, the way a unified diff shows them. An unchanged
/// configuration of thousands of lines is not what a caller asked a diff for.
pub(crate) fn diff_context(changed: &[bool], context: usize) -> Vec<bool> {
    let mut keep = vec![false; changed.len()];
    for (i, _) in changed.iter().enumerate().filter(|(_, c)| **c) {
        let lo = i.saturating_sub(context);
        let hi = (i + context).min(changed.len().saturating_sub(1));
        for k in keep.iter_mut().take(hi + 1).skip(lo) {
            *k = true;
        }
    }
    keep
}

/// Lines of real-shaped configuration and what each must read as after [`redact_line`]. The
/// values are invented; the shapes are the vendors'. A new shape goes here, with its expected
/// output, before the rule changes.
#[cfg(test)]
const CASES: &[(&str, &str)] = &[
    // Cisco IOS / IOS-XE / NX-OS
    ("enable secret 9 $9$abc$def", "enable secret <redacted>"),
    (
        "username admin privilege 15 secret 5 $1$xyz$abc",
        "username admin privilege 15 secret <redacted>",
    ),
    (
        "username ops password 7 0822455D0A16",
        "username ops password <redacted>",
    ),
    (
        "snmp-server community s3cr3t RO 10",
        "snmp-server community <redacted>",
    ),
    (
        "snmp-server user mon grp v3 auth sha AuthPass1 priv aes 128 PrivPass1",
        "snmp-server user mon grp v3 auth <redacted>",
    ),
    (
        "tacacs-server key 7 060506324F41",
        "tacacs-server key <redacted>",
    ),
    (
        " neighbor 192.0.2.1 password 7 1511021F0725",
        " neighbor 192.0.2.1 password <redacted>",
    ),
    (
        " ip ospf message-digest-key 1 md5 7 13061E010803",
        " ip ospf message-digest-key <redacted>",
    ),
    (
        "crypto isakmp key 0 sharedsecret address 198.51.100.1",
        "crypto isakmp key <redacted>",
    ),
    (" key-string 7 104D000A0618", " key-string <redacted>"),
    (
        "radius-server host 192.0.2.5 auth-port 1812 acct-port 1813 key 7 0822",
        "radius-server host 192.0.2.5 auth-port <redacted>",
    ),
    (" wpa-psk ascii 0 wifipass", " wpa-psk <redacted>"),
    (
        "ntp authentication-key 1 md5 1234 7",
        "ntp authentication-key <redacted>",
    ),
    // Huawei VRP / YunShan
    (
        " local-user admin password irreversible-cipher $1c$abc$def",
        " local-user admin password <redacted>",
    ),
    (
        "snmp-agent community read cipher %^%#abcdef%^%#",
        "snmp-agent community <redacted>",
    ),
    (
        "snmp-agent usm-user v3 mon authentication-mode sha cipher %^%#x%^%#",
        "snmp-agent usm-user v3 mon authentication-mode sha cipher <redacted>",
    ),
    (
        " pre-shared-key cipher %@%@abc%@%@",
        " pre-shared-key <redacted>",
    ),
    // H3C Comware
    (" password hash $h$6$abc", " password <redacted>"),
    (
        "snmp-agent community read simple public",
        "snmp-agent community <redacted>",
    ),
    // Juniper Junos
    (
        "set system root-authentication encrypted-password \"$6$abc$def\"",
        "set system root-authentication encrypted-password <redacted>",
    ),
    (
        "    authentication-key \"$9$abcDEF\"; ## SECRET-DATA",
        "    authentication-key <redacted>",
    ),
    (
        "set security ike policy p pre-shared-key ascii-text \"$9$x\"",
        "set security ike policy p pre-shared-key <redacted>",
    ),
    // FortiGate: words run together, and `ENC` before a value
    (
        "    set psksecret ENC abcd==",
        "    set psksecret <redacted>",
    ),
    ("    set passwd ENC SH2abcdef", "    set passwd <redacted>"),
    (
        "    set private-data ENC xyz123",
        "    set private-data ENC <redacted>",
    ),
    // A community string with no word in front of it
    (
        "snmp-server host 192.0.2.10 version 2c c0mmun1ty",
        "snmp-server host <redacted>",
    ),
    // Credentials inside a URL
    (
        "archive path ftp://backup:Pa55w0rd@198.51.100.7/cfg",
        "archive path ftp://<redacted>@198.51.100.7/cfg",
    ),
    (
        " url https://example.com/path?q=1",
        " url https://example.com/path?q=1",
    ),
    // A password holding `/`, a URL that is itself the secret word, and a URL before a word
    (
        "archive path ftp://backup:Pa/55w0rd@198.51.100.7/cfg",
        "archive path ftp://<redacted>@198.51.100.7/cfg",
    ),
    (
        "copy ftp://backup:secretpw1@198.51.100.7/cfg startup-config",
        "copy <redacted>",
    ),
    (
        "fetch https://example.com/x?password=urlpw1 now",
        "fetch <redacted>",
    ),
    // A value shaped like a URL after a word, and a secret named in a query
    ("key=abc://valpw3", "key=<redacted>"),
    ("auth:foo://valpw4 more", "auth:<redacted>"),
    (
        "fetch https://example.com/x?id=1&key=qpw5",
        "fetch <redacted>",
    ),
    (
        "kron ftp://ops:urlpw2@192.0.2.9/a key 7 0822",
        "kron ftp://<redacted>@192.0.2.9/a key <redacted>",
    ),
    (" passive-interface default", " passive-interface default"),
    // key=value forms
    ("password=hunter2", "password=<redacted>"),
    ("api_key: abc123", "api_key:<redacted>"),
    // A hash with no word before it
    (
        " hashed-value $6$salt$digest end",
        " hashed-value <redacted> end",
    ),
    // Left alone: a word with nothing after it, or no secret word at all
    ("service password-encryption", "service password-encryption"),
    ("no password", "no password"),
    (" keepalive 10", " keepalive 10"),
    (
        "aaa authentication login default group tacacs+ local",
        "aaa authentication login default group tacacs+ local",
    ),
    (
        "interface GigabitEthernet0/1",
        "interface GigabitEthernet0/1",
    ),
    (
        " description uplink to core $ price",
        " description uplink to core $ price",
    ),
    (
        " ip address 192.0.2.1 255.255.255.0",
        " ip address 192.0.2.1 255.255.255.0",
    ),
    ("hostname sw-01", "hostname sw-01"),
    ("", ""),
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_case_reads_as_written() {
        assert!(CASES.len() >= 30, "the case table lost rows");
        let mut wrong = Vec::new();
        for (input, want) in CASES {
            let (got, changed) = redact_line(input);
            if got != *want || changed != (input != want) {
                wrong.push(format!("{input:?}\n   got  {got:?}\n   want {want:?}"));
            }
        }
        assert!(
            wrong.is_empty(),
            "redaction disagrees:\n{}",
            wrong.join("\n")
        );
    }

    #[test]
    fn no_blanked_case_keeps_its_secret_value() {
        // The values the table invented. None may survive, wherever on the line it sat.
        for needle in [
            "s3cr3t",
            "AuthPass1",
            "PrivPass1",
            "hunter2",
            "wifipass",
            "sharedsecret",
            "0822455D0A16",
            "$9$abc",
            "%^%#abcdef",
            "abcd==",
            "SH2abcdef",
            "xyz123",
            "c0mmun1ty",
            "Pa55w0rd",
            "55w0rd",
            "secretpw1",
            "urlpw1",
            "urlpw2",
            "valpw3",
            "valpw4",
            "qpw5",
        ] {
            for (input, _) in CASES {
                if input.contains(needle) {
                    assert!(
                        !redact_line(input).0.contains(needle),
                        "{needle} survived in {input:?}"
                    );
                }
            }
        }
    }

    #[test]
    fn a_private_key_block_is_blanked_between_its_markers() {
        let text =
            "a\n-----BEGIN RSA PRIVATE KEY-----\nMIIEow\nAAAB\n-----END RSA PRIVATE KEY-----\nb";
        let out: Vec<String> = redact_lines(text.lines())
            .into_iter()
            .map(|(l, _)| l)
            .collect();
        assert_eq!(
            out,
            [
                "a",
                "-----BEGIN RSA PRIVATE KEY-----",
                REDACTED,
                REDACTED,
                "-----END RSA PRIVATE KEY-----",
                "b"
            ]
        );
        // A key written on one line, and a value that starts on the header's own line, both lose
        // everything but the marker (the review of the first version found both passing through).
        let one_line = "-----BEGIN PRIVATE KEY----- MIIEsecretbody -----END PRIVATE KEY-----";
        assert_eq!(
            redact_lines([one_line].into_iter()),
            [(REDACTED.to_owned(), true)]
        );
        let junos = "    private-key \"-----BEGIN RSA PRIVATE KEY-----MIIEsecretbody\nAAAB\nlastbit-----END RSA PRIVATE KEY-----\";";
        let out: Vec<String> = redact_lines(junos.lines())
            .into_iter()
            .map(|(l, _)| l)
            .collect();
        assert!(
            out.iter().all(|l| !l.contains("MIIEsecretbody")
                && !l.contains("lastbit")
                && !l.contains("AAAB")),
            "{out:?}"
        );
        assert_eq!(out[2], format!("{REDACTED} -----END RSA PRIVATE KEY-----"));
        assert!(
            out[0].contains("-----BEGIN RSA PRIVATE KEY-----"),
            "{out:?}"
        );
        // A public certificate is not a secret and stays readable.
        let cert = "-----BEGIN CERTIFICATE-----\nMIIB\n-----END CERTIFICATE-----";
        assert!(redact_lines(cert.lines()).iter().all(|(_, c)| !c));
    }

    #[test]
    fn a_match_pattern_is_bounded_and_must_compile() {
        assert!(compile_match("^interface ").is_ok());
        assert!(compile_match("(").is_err());
        assert!(compile_match(&"a".repeat(MATCH_MAX_CHARS + 1)).is_err());
    }

    #[test]
    fn a_page_counts_what_matched_and_says_when_more_follows() {
        let items: Vec<String> = (1..=10).map(|i| format!("line {i}")).collect();
        let p = page(items.clone(), |s| s.as_str(), None, 0, 4);
        assert_eq!((p.matched, p.lines.len(), p.truncated), (10, 4, true));
        let p = page(items.clone(), |s| s.as_str(), None, 8, 4);
        assert_eq!((p.lines.len(), p.truncated), (2, false));
        let re = compile_match("line 1").unwrap();
        let p = page(items.clone(), |s| s.as_str(), Some(&re), 0, 500);
        assert_eq!((p.matched, p.truncated), (2, false)); // "line 1" and "line 10"
                                                          // A limit past the ceiling is clamped, never honoured.
        let many: Vec<String> = (0..MAX_LIMIT + 10).map(|i| i.to_string()).collect();
        let p = page(many, |s| s.as_str(), None, 0, usize::MAX);
        assert_eq!(p.lines.len(), MAX_LIMIT);
        assert!(p.truncated);
    }

    #[test]
    fn a_pattern_runs_over_the_blanked_text_and_cannot_find_a_value() {
        let lines: Vec<String> = redact_lines(["snmp-server community s3cr3t RO"].into_iter())
            .into_iter()
            .map(|(l, _)| l)
            .collect();
        let re = compile_match("s3cr3t").unwrap();
        assert_eq!(page(lines, |s| s.as_str(), Some(&re), 0, 10).matched, 0);
    }

    #[test]
    fn a_diff_keeps_changes_and_their_context_only() {
        let changed = [
            false, false, false, false, true, false, false, false, false, false,
        ];
        let keep = diff_context(&changed, 2);
        assert_eq!(
            keep,
            [false, false, true, true, true, true, true, false, false, false]
        );
        assert!(diff_context(&[false; 5], 3).iter().all(|k| !k));
        assert!(diff_context(&[], 3).is_empty());
    }
}
