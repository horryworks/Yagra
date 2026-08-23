// SPDX-License-Identifier: AGPL-3.0-only
//! Reading a module's **own source text**, whether it is one file or a directory (ADR-089/091).
//!
//! Some things a module must hold true have no type, so the only check available is to read the
//! source as a string: that a `#[tool]` names itself, that a SQL statement interpolates an enum
//! instead of a literal, that an analysis short-circuits on the store it declares it needs. Those
//! checks are legitimate — and each one says why it is the only technique available — but they
//! share a failure mode that has now bitten this repository five times.
//!
//! **A reader left pointing at one file of several keeps running, over a fraction, reporting
//! success.** ADR-086 met that when `mcp/tools.rs` became `mcp/tools/`, and wrote
//! `mcp/tool_source.rs` for it. ADR-089 is the same split on `analysis.rs`, so the mechanism moved
//! here rather than being written a second time. ADR-091 is why this is *one* place rather than
//! twenty-three: the rule below was hand-copied into every file that checks its own SQL, and every
//! copy — this one included — carried the same defect.
//!
//! Three behaviours live here because none of them is about MCP or about analysis:
//!
//! 1. **Both spellings of a root.** `X.rs` *and* `X/` are looked for, so this needs no edit on the
//!    day of a split — and a half-finished split (both present) is read whole rather than half.
//! 2. **Every test-only item is removed, per file.** 🚨 This is the load-bearing one, and ADR-091
//!    had to rewrite it. Doing it caller-side over a *concatenation* keeps only the first file's
//!    code — measured on ADR-086, where one guard checked 10 tools of 36 and passed its own
//!    assertions. Worse, the old spelling of it was wrong even for a single file; see below.
//! 3. **Test-only modules are excluded, derived from `mod.rs`.** A guard file sitting *inside* the
//!    directory it greps matches its own needles. ADR-086 hit that within minutes of splitting.
//!    Derived from the `#[cfg(test)] mod …` lines rather than listed, for the same reason the file
//!    list is derived: a name written down is a name that can be forgotten.
//!
//! ## The rule, and why it is not "cut at the first `#[cfg(test)]`"
//!
//! **Code is the file with every top-level test-only *item* taken out** — not the prefix above the
//! first one. Cutting truncates the moment a test-only `use`, `fn`, `type` or a second inline test
//! module appears anywhere but the very end, and **ten files in this crate are already shaped that
//! way**. `logstore.rs` opens with a test-only `use std::sync::Mutex;` on line 16, so the old rule
//! called **15 lines of 2,606** its code and every needle over it was vacuous. `config.rs` is the
//! case that also defeats the obvious repair: it holds *two* inline test modules with two
//! production functions between them, so "cut at the first inline `mod … {`" loses those two too.
//!
//! Two limits are deliberate, and both are stated here because a reader would otherwise assume
//! otherwise:
//!
//! * **Top level only.** An indented attribute — a test-only method inside an `impl`, of which this
//!   workspace has sixteen — stays in the code. This mechanism is line-oriented, and rustfmt (a CI
//!   gate *and* a `/flashdeploy` pre-commit guard) is what guarantees a top-level item starts and
//!   ends at column zero. Nothing guarantees that about a member.
//! * **When the end is ambiguous, too much is kept rather than too little.** A raw string holding a
//!   column-zero `}` ends an item early, leaving some test text in the code. That makes a check
//!   *noisy*; the opposite mistake makes a check *silent*, and only silence is fatal. Every
//!   ambiguity here is resolved toward noise on purpose.
//!
//! ## The floor is the caller's; the invariant is this module's
//!
//! **The floor stays out of here.** Each caller asserts its own minimum — the MCP surface counts
//! `#[tool(`, the analysis module counts `async fn run_`. Sharing "how to read" removes a
//! duplicate; sharing "how many there should be" would hide what is being counted from the place
//! that cares, and a floor nobody can see is a floor nobody maintains.
//!
//! What *is* asserted here is a different kind of claim: not "did the caller get enough" but **"did
//! this mechanism cut in a sane place"** — no top-level test attribute survives, and a non-empty
//! file does not come back empty. Those are statements about the machinery, so they live with the
//! machinery, and `every_file_in_this_crate_survives_being_read` checks them over all of the crate's
//! real files rather than over three synthetic strings.
//!
//! 🚨 **Those two invariants only catch one of the two directions, and this was measured rather
//! than assumed.** They fire when the mechanism leaves test text *behind*. They are silent when it
//! takes *too much* — restoring the old "cut at the first attribute" rule satisfies both of them on
//! every file in the crate, because a truncated file still holds no attribute and is still
//! non-empty. The over-cut direction is guarded by naming two real files instead:
//! `the_two_files_that_were_being_mis_read_come_back_whole` asserts that `config.rs` still has the
//! two functions between its test modules and that `logstore.rs` still has its `impl LogStore`.
//! A general assertion for that direction would have to know what the file is *supposed* to
//! contain, which is the caller's floor again — so this is the boundary, not an omission.
//!
//! Everything here is test-only. It reads from disk (`CARGO_MANIFEST_DIR`) rather than
//! `include_str!` because a macro needs a literal path and there is no literal that means "every
//! file of this module". `api/route_table.rs::declared_mcp_tools` has always read this way.

use std::path::{Path, PathBuf};

/// The attribute this module recognises: on a line of its own, at column zero.
///
/// A `const` rather than a runtime-assembled needle, unlike the ones in files that grep themselves:
/// nothing here searches for a *banned* pattern, so this file's own occurrences of the literal
/// cannot satisfy anything. What matters is that the comparison is against a whole line — the line
/// you are reading holds the text and is not equal to it.
const MARK: &str = "#[cfg(test)]";

/// Both spellings of a module root: `<dir>/<stem>.rs` and `<dir>/<stem>/`, whichever exist.
///
/// `dir` is relative to the crate root (e.g. `"src/mcp"`), and may climb out of it
/// (`"../yagra-poller/src"`) — `api/metrics.rs` reads the poller's worker that way.
pub(crate) fn roots(dir: &str, stem: &str) -> Vec<PathBuf> {
    let base = Path::new(env!("CARGO_MANIFEST_DIR")).join(dir);
    [base.join(format!("{stem}.rs")), base.join(stem)]
        .into_iter()
        .filter(|p| p.exists())
        .collect()
}

/// The whole module's code, concatenated — the form most callers want.
///
/// Prefer [`files`] when a finding names the file or the function it was found in: a per-function
/// scan over the concatenation attributes the second file's items to the first file's last
/// function.
pub(crate) fn code(dir: &str, stem: &str) -> String {
    let roots = roots(dir, stem);
    assert!(
        !roots.is_empty(),
        "no module `{stem}` under {dir}: neither {stem}.rs nor {stem}/ exists, so every check \
         built on this text would run over nothing"
    );
    files(&roots)
        .into_iter()
        .map(|(_, text)| text)
        .collect::<Vec<_>>()
        .join("\n")
}

/// [`code`] with whole-line `//` comments dropped.
///
/// Two things would otherwise make a "this pattern must not appear" assertion useless: a test that
/// reads its own file matches its own needles, and a doc comment *naming* the banned pattern —
/// "never OFFSET" — reads as the pattern itself. Eighteen files asked for exactly this before
/// ADR-091, each with its own copy of the filter and its own copy of the defect above.
pub(crate) fn code_no_comments(dir: &str, stem: &str) -> String {
    code(dir, stem)
        .lines()
        .filter(|l| !l.trim_start().starts_with("//"))
        .collect::<Vec<_>>()
        .join("\n")
}

/// The modules a root declares as test-only, as file names.
///
/// The declaring file is `<root>/mod.rs` for a directory root and the root itself for a file root.
/// ⚠️ **Both cases are needed, and the file case is the one that is easy to miss**: a module that is
/// still a single `X.rs` can already own `X/guards.rs`, which is exactly the state a split passes
/// through. Resolving only `mod.rs` would let the guard file into the text it greps for the length
/// of the increment that exists to stop that.
///
/// Reads the **raw** file rather than its code: the declarations it looks for are the very items
/// [`strip_test_items`] takes out.
pub(crate) fn test_only_modules(root: &Path) -> Vec<String> {
    let declarer = if root.is_dir() {
        root.join("mod.rs")
    } else {
        root.to_path_buf()
    };
    if !declarer.exists() {
        return Vec::new();
    }
    read(&declarer)
        .split(MARK)
        .skip(1)
        .filter_map(declared_module)
        .map(|m| format!("{m}.rs"))
        .collect()
}

/// The module name in `pub(crate) mod x;` at the start of `after`, if that is what it is.
///
/// `None` for an inline `mod tests { … }` — that is a test *body*, not a declaration, and treating
/// it as one yielded a garbage "module name" running to the first semicolon of its contents.
fn declared_module(after: &str) -> Option<String> {
    let t = after.trim_start();
    let t = t.strip_prefix("pub(crate) ").unwrap_or(t);
    let t = t.strip_prefix("pub(super) ").unwrap_or(t);
    let t = t.strip_prefix("pub ").unwrap_or(t);
    let rest = t.strip_prefix("mod ")?;
    let name = rest.split(';').next()?.trim();
    (!name.is_empty() && name.chars().all(|c| c.is_alphanumeric() || c == '_'))
        .then(|| name.to_owned())
}

/// Does this line close a top-level item?
///
/// Only a column-zero line can, which is what rustfmt buys: everything nested is indented, so the
/// first unindented `}` / `};` / `…;` after an item's head is that item's end.
fn closes_at_column_zero(line: &str) -> bool {
    if line.is_empty() || line.starts_with(char::is_whitespace) {
        return false;
    }
    line == "}" || line.trim_end().ends_with(';')
}

/// Index of the last line of the test-only item whose attribute sits at `at`.
fn item_end(lines: &[&str], at: usize) -> usize {
    // Further attributes belong to the same item — `#[cfg(test)] #[must_use] fn …` is real
    // (`api/openapi.rs`), and reading the `#[must_use]` as the item's head would end it there.
    let mut head = at + 1;
    while head < lines.len() && lines[head].starts_with("#[") {
        head += 1;
    }
    if head >= lines.len() {
        return lines.len() - 1;
    }
    // A one-line statement item (`use …;`, `mod x;`, `type X = …;`) ends on its own line.
    if closes_at_column_zero(lines[head]) {
        return head;
    }
    // Anything else runs to the next line that closes at column zero. Running off the end means the
    // file ends inside the item, so the item is the rest of the file.
    lines[head + 1..]
        .iter()
        .position(|l| closes_at_column_zero(l))
        .map_or(lines.len() - 1, |off| head + 1 + off)
}

/// A file's code: the text with every top-level test-only item removed.
///
/// See the module doc for why this removes items rather than cutting at the first one, and for the
/// two deliberate limits (top level only; ambiguity keeps text rather than dropping it).
fn strip_test_items(text: &str) -> String {
    let lines: Vec<&str> = text.lines().collect();
    let mut kept: Vec<&str> = Vec::with_capacity(lines.len());
    let mut i = 0;
    while i < lines.len() {
        if lines[i] == MARK {
            i = item_end(&lines, i) + 1;
        } else {
            kept.push(lines[i]);
            i += 1;
        }
    }
    let mut out = kept.join("\n");
    if text.ends_with('\n') {
        out.push('\n');
    }
    out
}

/// [`strip_test_items`] plus this module's own invariants — see the module doc's last section.
///
/// These are not a floor (the caller owns that). They answer "did the mechanism cut in a sane
/// place", which is a question about the machinery and has no caller-visible subject.
fn strip_and_check(name: &str, text: &str) -> String {
    let code = strip_test_items(text);
    assert!(
        !code.lines().any(|l| l == MARK),
        "{name}: a top-level `{MARK}` item survived being stripped, so a needle can match a \
         test's own literal and a count can be inflated by test-only code"
    );
    assert!(
        text.trim().is_empty() || !code.trim().is_empty(),
        "{name}: reading it left no code at all; every check built on this file is now vacuous \
         while reporting success"
    );
    code
}

/// Every file of the module as `(file name, code)`, sorted by name.
///
/// The contents are the **code**: each file with its own test-only items removed, and the modules
/// that are test-only in full left out entirely. Every caller wants it that way, and doing it here
/// means no caller writes the cut for itself — see behaviour 2 in the module doc.
///
/// Sorted so a caller that reports a finding names the same file every run; `read_dir` order is not
/// stable across platforms and an unstable message reads as a flaky test.
pub(crate) fn files(roots: &[PathBuf]) -> Vec<(String, String)> {
    let skip: Vec<String> = roots.iter().flat_map(|r| test_only_modules(r)).collect();
    let mut out: Vec<(String, String)> = Vec::new();
    let mut push = |name: String, text: String| {
        if skip.contains(&name) {
            return;
        }
        let code = strip_and_check(&name, &text);
        out.push((name, code));
    };
    for root in roots {
        if root.is_file() {
            let name = file_name(root);
            let text = read(root);
            push(name, text);
            continue;
        }
        for entry in
            std::fs::read_dir(root).unwrap_or_else(|e| panic!("read {}: {e}", root.display()))
        {
            let path = entry.expect("a directory entry").path();
            if path.extension().and_then(|e| e.to_str()) != Some("rs") {
                continue;
            }
            let name = file_name(&path);
            let text = read(&path);
            push(name, text);
        }
    }
    out.sort_by(|a, b| a.0.cmp(&b.0));
    out
}

fn file_name(path: &Path) -> String {
    path.file_name()
        .and_then(|f| f.to_str())
        .expect("a UTF-8 file name")
        .to_owned()
}

fn read(path: &Path) -> String {
    std::fs::read_to_string(path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every `.rs` file under a directory, recursively.
    fn rs_files(dir: &Path, out: &mut Vec<PathBuf>) {
        for entry in std::fs::read_dir(dir).unwrap_or_else(|e| panic!("read {dir:?}: {e}")) {
            let path = entry.expect("a directory entry").path();
            if path.is_dir() {
                rs_files(&path, out);
            } else if path.extension().and_then(|e| e.to_str()) == Some("rs") {
                out.push(path);
            }
        }
    }

    /// A root that is a plain file and a root that is a directory are both found, and a stem that
    /// is neither yields nothing rather than panicking.
    ///
    /// 🚨 **Each case asserts the root's shape, not just that one was found, and that is the whole
    /// point of the test.** It used to name `repo` as the file case and check only
    /// `len() == 1`. `roots` returns one path for either spelling, so the day `repo.rs` became
    /// `repo/` (ADR-094) the count would still have been 1, the message would have read
    /// "repo.rs is a file" about a directory, and **this test would have had two directory cases
    /// and no file case while staying green** — the failure mode this module exists to prevent,
    /// inside the module itself. Changing the example alone would only move that hole to the next
    /// file to be split; asserting the shape closes it wherever the example ends up.
    #[test]
    fn a_root_is_found_whether_it_is_a_file_or_a_directory() {
        let dir = roots("src/mcp", "tools");
        assert_eq!(dir.len(), 1, "tools/ is a directory");
        assert!(dir[0].is_dir(), "the directory case must really be one");
        let file = roots("src", "seed_ids");
        assert_eq!(file.len(), 1, "seed_ids.rs is a plain file");
        assert!(file[0].is_file(), "the file case must really be one");
        assert!(
            roots("src", "no_such_module_exists").is_empty(),
            "a stem naming nothing must yield nothing, not a phantom path"
        );
    }

    /// The exclusion is derived from `mod.rs`, and it excludes the file that would otherwise match
    /// its own needles.
    #[test]
    fn the_test_only_exclusion_is_derived_from_mod_rs() {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("src/mcp/tools");
        let skip = test_only_modules(&dir);
        assert!(
            skip.contains(&"guards.rs".to_owned()),
            "guards.rs sits inside the directory it greps and must be excluded; got {skip:?}"
        );
        // …and a module with no `mod.rs` answers "nothing", not "everything".
        let none =
            test_only_modules(&Path::new(env!("CARGO_MANIFEST_DIR")).join("src/no_such_dir"));
        assert!(none.is_empty());
    }

    /// A file's test module is taken away, and it is taken **per file** rather than once over the
    /// concatenation — the difference this module exists for.
    #[test]
    fn every_file_is_cut_at_its_own_test_module() {
        let got = files(&roots("src/mcp", "tools"));
        assert!(got.len() >= 5, "only {} files were read", got.len());
        for (name, code) in &got {
            assert!(
                !code.lines().any(|l| l == MARK),
                "{name}: its test module survived, so a needle can match a test's own literal"
            );
        }
        // The proof that the cut is per file: a file *after* the first still has its code. Cutting
        // the concatenation once would leave everything past the first attribute empty.
        let last = got.last().expect("at least one file");
        assert!(
            !last.1.trim().is_empty(),
            "{}: the last file came back empty — the cut was applied to the concatenation",
            last.0
        );
    }

    /// **The defect ADR-091 exists for: an item is removed, the file is not truncated at it.**
    ///
    /// Acceptance side first — the test module really does go — because a helper that removed
    /// nothing at all would satisfy the interesting half by accident.
    #[test]
    fn a_test_only_item_removes_itself_and_nothing_below_it() {
        let src = "use a::b;\n#[cfg(test)]\nuse std::sync::Mutex;\nfn keep() {}\n#[cfg(test)]\n\
                   mod tests {\n    fn t() {}\n}\n";
        let code = strip_test_items(src);
        assert!(
            !code.contains("mod tests"),
            "the test module must still go: {code:?}"
        );
        assert!(
            !code.contains("Mutex"),
            "the test-only import must go too: {code:?}"
        );
        assert!(
            code.contains("fn keep()"),
            "the production function below the test-only import was lost — the exact defect this \
             replaced, which read `logstore.rs` as 15 lines of 2,606: {code:?}"
        );

        // Two inline test modules with production code between them: `config.rs`'s shape, and the
        // reason "cut at the first inline `mod … {`" was not a sufficient repair either.
        let two = "#[cfg(test)]\nmod first {\n}\nfn between() {}\n#[cfg(test)]\nmod tests {\n}\n";
        let code = strip_test_items(two);
        assert!(code.contains("fn between()"), "{code:?}");
        assert!(!code.contains("mod first"), "{code:?}");

        // A stacked attribute belongs to the same item (`api/openapi.rs`'s shape).
        let stacked = "#[cfg(test)]\n#[must_use]\nfn helper() -> u8 {\n    1\n}\nfn keep() {}\n";
        let code = strip_test_items(stacked);
        assert!(!code.contains("fn helper()"), "{code:?}");
        assert!(code.contains("fn keep()"), "{code:?}");

        // …and a file that is all code comes back whole.
        assert_eq!(strip_test_items("fn a() {}\n"), "fn a() {}\n");
    }

    /// A `#[cfg(test)] mod x;` declaration is an item like any other, and taking it out must not
    /// take the code below it.
    ///
    /// It used to be special-cased — skipped, so it stayed in the text — which is why one such line
    /// at the top of `main.rs` truncated its production text to the imports under the old rule.
    #[test]
    fn a_test_only_module_declaration_is_not_where_the_code_ends() {
        let src = "mod x;\n#[cfg(test)]\nmod guards;\nfn a() {}\n#[cfg(test)]\nmod tests {\n}\n";
        let code = strip_test_items(src);
        assert!(
            code.contains("fn a()"),
            "the declaration truncated the code half; that is how one line at the top of main.rs \
             cut its production text to its imports: {code:?}"
        );
        assert!(code.contains("mod x;"), "{code:?}");
        assert!(!code.contains("mod guards;"), "{code:?}");
        // The exclusion list still sees it: it reads the raw file, not the code.
        assert_eq!(
            declared_module("\nmod guards;\nfn a() {}"),
            Some("guards".to_owned())
        );
    }

    /// **Two files this crate really holds, read back whole.**
    ///
    /// Synthetic strings prove the algorithm; these two prove it against the shapes that were
    /// actually being mis-read, and they name the files so a regression says where to look.
    #[test]
    fn the_two_files_that_were_being_mis_read_come_back_whole() {
        // `config.rs`: two inline test modules with production code between them.
        let cfg = code("src", "config");
        for needle in ["fn interval_in_bounds(", "fn parse_interval("] {
            assert!(
                cfg.contains(needle),
                "config.rs is missing `{needle}`, which sits between its two test modules"
            );
        }
        assert!(!cfg.contains("mod system_log_days_tests"), "…and both go");

        // `logstore.rs`: a test-only `use` on line 16 used to end the file there.
        let logs = code("src", "logstore");
        assert!(
            logs.contains("impl LogStore for VlStore"),
            "logstore.rs was read as its first few lines again — the production impl is gone"
        );
        assert!(
            logs.lines().count() > 500,
            "logstore.rs came back as {} lines; it used to come back as 15",
            logs.lines().count()
        );
    }

    /// **Nobody computes "production code" for themselves any more.**
    ///
    /// Twenty-three files did before ADR-091, each holding its own copy of a rule that was wrong,
    /// and the copies were invisible: a hand-rolled cut compiles, runs, and reports success over a
    /// fraction of a file. The needle is the attribute **as a string literal** rather than any one
    /// spelling of the cut, because `.split`, `.split_once` and `.find` were all in use and a
    /// fourth is one keystroke away.
    ///
    /// Comment lines are dropped before the search: prose has to be able to name the thing it
    /// forbids, and this module's own doc does so repeatedly.
    #[test]
    fn no_file_spells_the_test_attribute_for_itself() {
        let needle = format!("\"{MARK}\"");
        // Acceptance side first: a search whose needle has stopped matching is indistinguishable
        // from a clean crate (`rejection-only-tests-pass-when-everything-rejects`).
        assert!(
            format!("SRC.split({needle}).next()").contains(&needle),
            "the needle no longer matches the idiom it exists to find"
        );

        let mut paths = Vec::new();
        rs_files(
            &Path::new(env!("CARGO_MANIFEST_DIR")).join("src"),
            &mut paths,
        );
        // …except this module, which is the one place the rule is allowed to be written down.
        let searched: Vec<&PathBuf> = paths
            .iter()
            .filter(|p| file_name(p) != "module_source.rs")
            .collect();
        // 🚨 The floor counts what was **searched**, not what was walked. Asserting on `paths`
        // before the exclusion read identically and was not the same thing: narrowing the exclusion
        // until it matched everything left this test green over zero files, which is the failure it
        // exists to stop. Found by breaking it, which is the only way this kind is ever found.
        assert!(
            searched.len() >= 150,
            "only {} files were searched; nothing below is being checked",
            searched.len()
        );
        let offenders: Vec<String> = searched
            .iter()
            .filter(|p| {
                read(p)
                    .lines()
                    .filter(|l| !l.trim_start().starts_with("//"))
                    .any(|l| l.contains(&needle))
            })
            .map(|p| file_name(p))
            .collect();
        assert!(
            offenders.is_empty(),
            "{offenders:?} spell the test attribute themselves. Read the module through \
             `module_source::code` or `code_no_comments` instead: a hand-rolled cut is how ten \
             files in this crate came to be read as 1–14% of themselves, and the reader could not \
             tell"
        );
    }

    /// **The mechanism's own invariants, over every real file in this crate.**
    ///
    /// Three synthetic strings say the algorithm is right about the shapes I thought of. This says
    /// it is right about the shapes that exist — and it is the check that fires on the day someone
    /// writes a fourth shape, rather than the day a needle silently stops matching.
    #[test]
    fn every_file_in_this_crate_survives_being_read() {
        let mut paths = Vec::new();
        rs_files(
            &Path::new(env!("CARGO_MANIFEST_DIR")).join("src"),
            &mut paths,
        );
        assert!(
            paths.len() >= 150,
            "only {} files were walked; the walk stopped finding this crate's sources and every \
             assertion below is now vacuous",
            paths.len()
        );
        for path in &paths {
            // `strip_and_check` holds the two invariants; calling it *is* the assertion.
            let _ = strip_and_check(&file_name(path), &read(path));
        }
    }
}
