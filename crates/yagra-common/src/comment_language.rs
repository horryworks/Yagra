// SPDX-License-Identifier: AGPL-3.0-only
//! Every comment in the repository is written in English (ADR-190). Test-only.
//!
//! The repository is public, and a comment is read by whoever opens the file — and a `///` on an
//! API handler is also published verbatim in the OpenAPI document. The rule had been an instruction
//! to the people writing code and nothing more, and the tree had drifted: design-doc vocabulary
//! (the Japanese words for "decision", "increment", "rework") and whole Japanese phrases had reached
//! comments in both halves of the tree, one of them the published API description.
//!
//! **The check is "every character is one English prose uses", not "no Japanese".** A deny-list of
//! scripts answers the one language it names; an allow-list answers the question that was asked.
//! What English technical prose uses, beyond ASCII: accented Latin letters (a loanword or a name),
//! Greek letters (maths), and the symbol blocks this tree already leans on — dashes and quotes,
//! arrows, maths operators, box drawing, geometric shapes, and emoji. Ideographic punctuation and
//! fullwidth forms are *not* on it: corner brackets and fullwidth parentheses are what a Japanese
//! sentence looks like even when every word in it is Latin.
//!
//! The WebUI's half is `web/src/commentLanguage.test.ts`, which uses the TypeScript parser rather
//! than a hand lexer. **The character rule is written twice, once per language, and the two are
//! pinned to each other**: `the_allowed_ranges_are_the_webuis` below reads that file's table.
//!
//! What this reads:
//!
//! - every `.rs` file under `crates/` — production, tests and fixtures alike, because a comment in a
//!   test is read by the same people;
//! - the comments of every migration **after** [`LAST_GRANDFATHERED_MIGRATION`]. The ones before it
//!   cannot be edited at all — `sqlx::migrate!` checksums each file, and changing one byte of an
//!   applied migration stops every existing deployment from starting;
//! - whole-line `#` comments in the shell scripts, the Dockerfiles, the compose files and the CI
//!   workflows.
//!
//! What it does not read: string literals (a JA locale string, a Japanese syslog fixture), the
//! vendored `snmp2` (not ours), and the generated `web/src/api/` (its text comes from the Rust
//! doc comments this file already checks).

use std::path::{Path, PathBuf};

use crate::srcread;

/// The characters an English comment may contain beyond ASCII, as inclusive code-point ranges.
///
/// ⚠️ `web/src/commentLanguage.test.ts` holds the same table; a test below compares the two.
const ALLOWED_RANGES: &[(u32, u32)] = &[
    (0x00A0, 0x017F),   // Latin-1 Supplement + Latin Extended-A: é, ü, ×, °, ±, µ, §
    (0x0370, 0x03FF),   // Greek: α, Δ, Σ in maths
    (0x2000, 0x2BFF),   // punctuation, arrows, maths, technical, box drawing, shapes, symbols
    (0xFE00, 0xFE0F),   // variation selectors (the emoji form of ⚠)
    (0x1F000, 0x1FAFF), // emoji
];

/// The newest migration written before this check existed. Every migration after it is checked.
const LAST_GRANDFATHERED_MIGRATION: u32 = 143;

fn is_english(c: char) -> bool {
    let cp = c as u32;
    cp < 0x80
        || ALLOWED_RANGES
            .iter()
            .any(|&(lo, hi)| (lo..=hi).contains(&cp))
}

/// One comment, split into its lines: `(1-based line number, text of the comment on that line)`.
type CommentLines = Vec<(usize, String)>;

/// The comments of a Rust source file, with the line each piece of comment text sits on.
///
/// A lexer, not a line filter: `"http://…"` is not a comment and `/* … */` can span lines. It knows
/// the four things that can hide a `//` or a `"` from a naive scan — string literals with escapes,
/// raw strings (`r#"…"#`), character literals (`'"'`, `'\''`) as distinct from lifetimes (`'a`),
/// and nested block comments.
fn rust_comments(src: &str) -> CommentLines {
    let chars: Vec<char> = src.chars().collect();
    let mut out: CommentLines = Vec::new();
    let mut line = 1;
    let mut i = 0;
    let push = |out: &mut CommentLines, line: usize, c: char| match out.last_mut() {
        Some((l, text)) if *l == line => text.push(c),
        _ => out.push((line, c.to_string())),
    };
    let ident = |c: char| c.is_alphanumeric() || c == '_';
    while i < chars.len() {
        let c = chars[i];
        let next = chars.get(i + 1).copied();
        if c == '\n' {
            line += 1;
            i += 1;
        } else if c == '/' && next == Some('/') {
            while i < chars.len() && chars[i] != '\n' {
                push(&mut out, line, chars[i]);
                i += 1;
            }
        } else if c == '/' && next == Some('*') {
            let mut depth = 0;
            while i < chars.len() {
                if chars[i] == '/' && chars.get(i + 1) == Some(&'*') {
                    depth += 1;
                    push(&mut out, line, '/');
                    push(&mut out, line, '*');
                    i += 2;
                } else if chars[i] == '*' && chars.get(i + 1) == Some(&'/') {
                    depth -= 1;
                    push(&mut out, line, '*');
                    push(&mut out, line, '/');
                    i += 2;
                    if depth == 0 {
                        break;
                    }
                } else {
                    if chars[i] == '\n' {
                        line += 1;
                    } else {
                        push(&mut out, line, chars[i]);
                    }
                    i += 1;
                }
            }
        } else if c == 'r'
            && (i == 0 || !ident(chars[i - 1]) || raw_prefix(&chars, i))
            && matches!(next, Some('"') | Some('#'))
        {
            let mut j = i + 1;
            let mut hashes = 0;
            while chars.get(j) == Some(&'#') {
                hashes += 1;
                j += 1;
            }
            if chars.get(j) != Some(&'"') {
                // `r#ident` — a raw identifier, not a string.
                i = j;
                continue;
            }
            j += 1;
            loop {
                match chars.get(j) {
                    None => break,
                    Some('\n') => line += 1,
                    Some('"') if (1..=hashes).all(|k| chars.get(j + k) == Some(&'#')) => {
                        j += 1 + hashes;
                        break;
                    }
                    _ => {}
                }
                j += 1;
            }
            i = j;
        } else if c == '"' {
            i += 1;
            while i < chars.len() && chars[i] != '"' {
                if chars[i] == '\\' {
                    i += 1;
                }
                if chars.get(i) == Some(&'\n') {
                    line += 1;
                }
                i += 1;
            }
            i += 1;
        } else if c == '\'' {
            if next == Some('\\') {
                // An escaped character literal (`'\''`, `'\u{1F6A8}'`): step over the quote, the
                // backslash and the escaped character, then on to the closing quote.
                i += 3;
                while i < chars.len() && chars[i] != '\'' {
                    i += 1;
                }
                i += 1;
            } else if chars.get(i + 2) == Some(&'\'') {
                i += 3;
            } else {
                // A lifetime or a label.
                i += 1;
            }
        } else {
            i += 1;
        }
    }
    out
}

/// Is the `r` at `i` the second letter of a `br"…"` / `cr"…"` prefix?
fn raw_prefix(chars: &[char], i: usize) -> bool {
    matches!(chars[i - 1], 'b' | 'c')
        && (i < 2 || !(chars[i - 2].is_alphanumeric() || chars[i - 2] == '_'))
}

/// The comments of a SQL file: `-- …` to the end of the line, and `/* … */`, outside `'…'`.
fn sql_comments(src: &str) -> CommentLines {
    let mut out = Vec::new();
    let mut in_block = false;
    for (n, text) in src.lines().enumerate() {
        let chars: Vec<char> = text.chars().collect();
        let mut comment = String::new();
        let mut in_str = false;
        let mut i = 0;
        while i < chars.len() {
            let c = chars[i];
            let next = chars.get(i + 1).copied();
            if in_block {
                comment.push(c);
                if c == '*' && next == Some('/') {
                    comment.push('/');
                    in_block = false;
                    i += 1;
                }
            } else if in_str {
                in_str = c != '\'';
            } else if c == '\'' {
                in_str = true;
            } else if c == '-' && next == Some('-') {
                comment.extend(&chars[i..]);
                break;
            } else if c == '/' && next == Some('*') {
                in_block = true;
                comment.push(c);
            }
            i += 1;
        }
        if !comment.is_empty() {
            out.push((n + 1, comment));
        }
    }
    out
}

/// Whole-line `#` comments. A `#` after code on the same line is left alone: in a shell script or a
/// YAML value it is as often part of a string as the start of a comment.
fn hash_comments(src: &str) -> CommentLines {
    src.lines()
        .enumerate()
        .filter(|(_, l)| {
            let t = l.trim_start();
            t.starts_with('#') && !t.starts_with("#!")
        })
        .map(|(n, l)| (n + 1, l.trim_start().to_string()))
        .collect()
}

/// The comment lines holding a character English does not use, as `path:line: text`.
fn offences(path: &str, comments: &CommentLines) -> Vec<String> {
    comments
        .iter()
        .filter(|(_, text)| !text.chars().all(is_english))
        .map(|(n, text)| format!("{path}:{n}: {}", text.trim()))
        .collect()
}

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn relative(root: &Path, p: &Path) -> String {
    p.strip_prefix(root)
        .unwrap_or(p)
        .to_string_lossy()
        .replace('\\', "/")
}

#[test]
fn every_comment_in_the_repository_is_english() {
    let root = repo_root();
    let mut found = Vec::new();

    let mut rs = Vec::new();
    srcread::rs_files(&root.join("crates"), &mut rs);
    let mut rust_comment_lines = 0;
    for p in &rs {
        let comments = rust_comments(&srcread::read(p));
        rust_comment_lines += comments.len();
        found.extend(offences(&relative(&root, p), &comments));
    }
    // Floors on what was *inspected*: a walk that stopped finding files, or a lexer that stopped
    // recognising comments, would otherwise pass as a clean tree.
    assert!(rs.len() >= 350, "only {} Rust files read", rs.len());
    assert!(
        rust_comment_lines >= 60_000,
        "only {rust_comment_lines} Rust comment lines found — the lexer has stopped seeing them"
    );

    let mut migrations = 0;
    for entry in std::fs::read_dir(root.join("migrations")).expect("migrations/ is readable") {
        let p = entry.expect("a readable directory entry").path();
        let name = srcread::file_name(&p);
        if !name.ends_with(".sql") {
            continue;
        }
        let number: u32 = name
            .split('_')
            .next()
            .and_then(|n| n.parse().ok())
            .unwrap_or_else(|| panic!("{name}: a migration is named NNNN_what.sql"));
        migrations += 1;
        if number > LAST_GRANDFATHERED_MIGRATION {
            found.extend(offences(
                &relative(&root, &p),
                &sql_comments(&srcread::read(&p)),
            ));
        }
    }
    assert!(migrations >= 140, "only {migrations} migrations read");

    let mut hash_files = Vec::new();
    for dir in ["scripts", "docker", ".github/workflows"] {
        collect(&root.join(dir), &mut hash_files);
    }
    for entry in std::fs::read_dir(&root).expect("the repository root is readable") {
        let p = entry.expect("a readable directory entry").path();
        let name = srcread::file_name(&p);
        if name.starts_with("docker-compose") && name.ends_with(".yml") {
            hash_files.push(p);
        }
    }
    hash_files.retain(|p| {
        let name = srcread::file_name(p);
        [".sh", ".py", ".yml", ".yaml", ".conf", "Dockerfile"]
            .iter()
            .any(|s| name.ends_with(s))
    });
    assert!(
        hash_files.len() >= 10,
        "only {} script/compose files read",
        hash_files.len()
    );
    for p in &hash_files {
        found.extend(offences(
            &relative(&root, p),
            &hash_comments(&srcread::read(p)),
        ));
    }

    assert!(
        found.is_empty(),
        "{} comment line(s) are not written in English (ADR-190) — rewrite them in English; a \
         Japanese string a comment needs to quote belongs in a string literal or a locale file:\n{}",
        found.len(),
        found.join("\n")
    );
}

fn collect(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries {
        let p = entry.expect("a readable directory entry").path();
        if p.is_dir() {
            collect(&p, out);
        } else {
            out.push(p);
        }
    }
}

#[test]
fn the_allowed_ranges_are_the_webuis() {
    let ts = srcread::read(&repo_root().join("web/src/commentLanguage.test.ts"));
    let start = ts
        .find("const ALLOWED_RANGES")
        .expect("commentLanguage.test.ts declares ALLOWED_RANGES");
    let end = start + ts[start..].find("];").expect("the table ends with `];`");
    let pairs: Vec<(u32, u32)> = ts[start..end]
        .lines()
        .filter_map(|l| {
            let l = l.trim();
            let inner = l.strip_prefix('[')?;
            let (lo, rest) = inner.split_once(',')?;
            let hi = rest.split(']').next()?;
            let hex = |s: &str| u32::from_str_radix(s.trim().trim_start_matches("0x"), 16).ok();
            Some((hex(lo)?, hex(hi)?))
        })
        .collect();
    assert_eq!(
        pairs, ALLOWED_RANGES,
        "the WebUI's comment check allows different characters from this one"
    );
}

#[test]
fn the_lexer_finds_comments_and_nothing_else() {
    let src = concat!(
        "let url = \"http://example.com\"; // trailing\n",
        "let raw = r#\"a // not a comment \"# ; /* block\n",
        "still block */ let q = '\"'; let e = '\\''; fn f<'a>() {}\n",
        "/* outer /* inner */ still outer */\n",
        "let s = \"escaped \\\" // quote\";\n",
        "/// doc\n",
    );
    let got: Vec<(usize, String)> = rust_comments(src);
    assert_eq!(
        got,
        vec![
            (1, "// trailing".to_string()),
            (2, "/* block".to_string()),
            (3, "still block */".to_string()),
            (4, "/* outer /* inner */ still outer */".to_string()),
            (6, "/// doc".to_string()),
        ]
    );
}

#[test]
fn english_prose_passes_and_japanese_does_not() {
    for ok in [
        "// ADR-164 decision 18 — the café’s “menu” → ✓ ⚠️ 🚨 ×2 ±1 °C α ▸ ─",
        "/// Settings ▸ Pollers … ≤ 5 ≥ 1",
    ] {
        assert!(ok.chars().all(is_english), "{ok}");
    }
    for bad in [
        "// ADR-164 \u{6c7a}\u{5b9a} 18",     // kanji
        "// \u{3067}\u{3059}",                // hiragana
        "// \u{300c}quoted\u{300d}",          // ideographic brackets
        "// width \u{ff08}fullwidth\u{ff09}", // fullwidth parentheses
        "// \u{043f}\u{0440}\u{0438}",        // Cyrillic
    ] {
        assert!(!bad.chars().all(is_english), "{bad}");
    }
}

#[test]
fn sql_and_hash_comments_are_found() {
    let sql = "SELECT '--not' FROM t; -- a comment\n/* one\ntwo */ SELECT 1;\n";
    assert_eq!(
        sql_comments(sql),
        vec![
            (1, "-- a comment".to_string()),
            (2, "/* one".to_string()),
            (3, "two */".to_string()),
        ]
    );
    let sh = "#!/bin/sh\n# a comment\necho '#not'\n  # indented\n";
    assert_eq!(
        hash_comments(sh),
        vec![
            (2, "# a comment".to_string()),
            (4, "# indented".to_string())
        ]
    );
}
