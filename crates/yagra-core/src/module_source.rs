// SPDX-License-Identifier: AGPL-3.0-only
//! Reading a module's **own source text**, whether it is one file or a directory (ADR-089).
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
//! here rather than being written a second time — and the two callers keep only the parts that are
//! genuinely theirs.
//!
//! Three behaviours live here because none of them is about MCP or about analysis:
//!
//! 1. **Both spellings of a root.** `X.rs` *and* `X/` are looked for, so this needs no edit on the
//!    day of a split — and a half-finished split (both present) is read whole rather than half.
//! 2. **Each file is cut at its *own* `#[cfg(test)]`.** 🚨 This is the load-bearing one. Doing it
//!    caller-side over a *concatenation* keeps only the first file's code — measured on ADR-086,
//!    where one guard checked 10 tools of 36 and passed its own assertions. `analysis.rs` carries
//!    exactly that shape today (`the_job_state_sql_is_built_from_the_enum`), which is why it is
//!    done here, once, where no caller can forget it.
//! 3. **Test-only modules are excluded, derived from `mod.rs`.** A guard file sitting *inside* the
//!    directory it greps matches its own needles. ADR-086 hit that within minutes of splitting.
//!    Derived from the `#[cfg(test)] mod …` lines rather than listed, for the same reason the file
//!    list is derived: a name written down is a name that can be forgotten.
//!
//! **What is deliberately *not* here: the floor.** Each caller asserts its own minimum — the MCP
//! surface counts `#[tool(`, the analysis module counts `async fn run_`. Sharing "how to read"
//! is removing a duplicate; sharing "how many there should be" would hide what is being counted
//! from the place that cares, and a floor nobody can see is a floor nobody maintains.
//!
//! Everything here is test-only. It reads from disk (`CARGO_MANIFEST_DIR`) rather than
//! `include_str!` because a macro needs a literal path and there is no literal that means "every
//! file of this module". `api/route_table.rs::declared_mcp_tools` has always read this way.

use std::path::{Path, PathBuf};

/// Both spellings of a module root: `<dir>/<stem>.rs` and `<dir>/<stem>/`, whichever exist.
///
/// `dir` is relative to the crate root (e.g. `"src/mcp"`).
pub(crate) fn roots(dir: &str, stem: &str) -> Vec<PathBuf> {
    let base = Path::new(env!("CARGO_MANIFEST_DIR")).join(dir);
    [base.join(format!("{stem}.rs")), base.join(stem)]
        .into_iter()
        .filter(|p| p.exists())
        .collect()
}

/// The modules a root declares under `#[cfg(test)]`, as file names.
///
/// The declaring file is `<root>/mod.rs` for a directory root and the root itself for a file root.
/// ⚠️ **Both cases are needed, and the file case is the one that is easy to miss**: a module that is
/// still a single `X.rs` can already own `X/guards.rs`, which is exactly the state a split passes
/// through. Resolving only `mod.rs` would let the guard file into the text it greps for the length
/// of the increment that exists to stop that.
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
        .split("#[cfg(test)]")
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

/// Where a file's test tail begins: the first `#[cfg(test)]` that is **not** a module declaration.
///
/// 🚨 The distinction is load-bearing and was found the hard way. `#[cfg(test)] mod guards;` is part
/// of a module's structure and belongs above its code, but the idiom every reader here uses —
/// "everything before the first `#[cfg(test)]`" — then truncates the file to its imports. Adding one
/// such declaration to `main.rs` cut its production text to 63 lines and failed two of its
/// structural tests instantly, which is the behaviour those tests are for.
fn test_tail_start(text: &str) -> Option<usize> {
    const MARK: &str = "\n#[cfg(test)]";
    let mut from = 0;
    while let Some(rel) = text[from..].find(MARK) {
        let at = from + rel;
        if declared_module(&text[at + MARK.len()..]).is_none() {
            return Some(at);
        }
        from = at + MARK.len();
    }
    None
}

/// Every file of the module as `(file name, code)`, sorted by name.
///
/// The contents are the **code**: each file cut at its own `#[cfg(test)]`, and the modules that are
/// test-only in full left out entirely. Every caller wants it that way, and doing it here means no
/// caller writes `.split("#[cfg(test)]")` for itself — see behaviour 2 in the module doc.
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
        let code = match test_tail_start(&text) {
            Some(i) => text[..i].to_owned(),
            None => text,
        };
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

    /// A root that is a plain file and a root that is a directory are both found, and a stem that
    /// is neither yields nothing rather than panicking.
    #[test]
    fn a_root_is_found_whether_it_is_a_file_or_a_directory() {
        assert_eq!(roots("src/mcp", "tools").len(), 1, "tools/ is a directory");
        assert_eq!(roots("src", "repo").len(), 1, "repo.rs is a file");
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

    /// A file's own `#[cfg(test)]` tail is cut away, and it is cut **per file** rather than once
    /// over the concatenation — the difference this module exists for.
    #[test]
    fn every_file_is_cut_at_its_own_test_module() {
        let got = files(&roots("src/mcp", "tools"));
        assert!(got.len() >= 5, "only {} files were read", got.len());
        for (name, code) in &got {
            assert!(
                test_tail_start(code).is_none(),
                "{name}: its test module survived, so a needle can match a test's own literal"
            );
        }
        // The proof that the cut is per file: a file *after* the first still has its code. Cutting
        // the concatenation once would leave everything past the first `#[cfg(test)]` empty.
        let last = got.last().expect("at least one file");
        assert!(
            !last.1.trim().is_empty(),
            "{}: the last file came back empty — the cut was applied to the concatenation",
            last.0
        );
    }

    /// A `#[cfg(test)] mod x;` **declaration** is structure, not a test tail.
    ///
    /// Acceptance side first: the cut must still happen where a test module really begins, or this
    /// would pass against a helper that never cuts anything at all.
    #[test]
    fn a_test_only_module_declaration_is_not_where_the_code_ends() {
        let real = "fn a() {}\n#[cfg(test)]\nmod tests {\n    fn t() {}\n}\n";
        assert_eq!(
            test_tail_start(real),
            Some("fn a() {}".len()),
            "the cut must still land on a real test module"
        );

        // The declaration is preceded by a line on purpose: the scan looks for `\n#[cfg(test)]`, so
        // a declaration on the very first byte would be skipped for the wrong reason and this test
        // would prove nothing.
        let with_decl =
            "mod x;\n#[cfg(test)]\nmod guards;\nfn a() {}\n#[cfg(test)]\nmod tests {\n}\n";
        let cut = test_tail_start(with_decl).expect("there is still a test module");
        assert!(
            with_decl[..cut].contains("fn a()"),
            "a `#[cfg(test)] mod x;` declaration truncated the code half; that is how one line at \
             the top of main.rs cut its production text to the imports"
        );

        // …and a file that is all code has no tail at all, rather than a cut at position 0.
        assert_eq!(test_tail_start("fn a() {}\n"), None);
    }
}
