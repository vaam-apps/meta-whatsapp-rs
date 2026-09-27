//! Deleting an entry of Meta's contact book cannot be undone
//! (`PhoneNumber::delete_contact_book_entry`, roadmap L9), so the library
//! promises that it never makes that call for its integrators: the
//! method's rustdoc and the CHANGELOG say nothing else in the library
//! deletes a contact book entry. This test keeps that promise true of the
//! library's sources.
//!
//! - **What it scans**: the `src/` of every library crate, which are the
//!   root manifest's `default-members`. The service's crates
//!   (`meta-whatsapp-server*`) are members but not default ones, and are
//!   not scanned: the service exposes the call to operators in roadmap
//!   M5c3. Every other member, and every directory under `crates/`, must
//!   be a default member, so no library crate goes unscanned.
//! - **What it skips**: comment lines, the lines of inline
//!   `#[cfg(test)]` modules (to their closing brace, not to the end of the
//!   file) and the files of out-of-line ones ([`library_lines`]).
//!   Everything else is scanned: a gated `impl` or `fn`, and any item
//!   after a test module.
//! - **The rule**: in what is left, the word `contact_book` is on exactly
//!   two lines, the method's signature and its path segment, plus the
//!   lines of the files in [`ALLOWED_CALLERS`]. A call from anywhere else,
//!   as a method or fully qualified, or a second path to the edge, fails.
//!
//! It is a text scan: an obfuscated path (`concat!("contact", "_book")`)
//! passes it, and review is the net for that.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::fs;
use std::path::{Path, PathBuf};

/// Library files, relative to the repository's root, that may name the
/// contact book besides the method itself: its callers. Empty, because
/// nothing in the library calls it. Roadmap M5c3 extends it for the
/// service: the service's own crates are not scanned, so an entry is only
/// needed for a library file its route goes through, with a comment saying
/// why that file may delete an entry.
const ALLOWED_CALLERS: &[&str] = &[];

/// The method's own file, and its two lines that name the contact book.
const OWN_FILE: &str = "crates/meta-whatsapp-client/src/phone_numbers/username.rs";
const OWN_SIGNATURE: &str = "pub async fn delete_contact_book_entry(";
const OWN_PATH_SEGMENT: &str = "\"contact_book\"])";

fn repo() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .unwrap()
}

fn rel(path: &Path) -> String {
    path.strip_prefix(repo())
        .unwrap_or(path)
        .to_string_lossy()
        .replace('\\', "/")
}

/// A list of paths of the root manifest's `[workspace]`: `members` or
/// `default-members`.
fn workspace_list(root: &Path, key: &str) -> Vec<String> {
    let manifest = fs::read_to_string(root.join("Cargo.toml")).unwrap();
    let start = manifest
        .find(&format!("\n{key} = ["))
        .unwrap_or_else(|| panic!("the root manifest lists its {key}"));
    let list = &manifest[start..];
    let list = &list[..list.find(']').unwrap()];
    list.split('"')
        .skip(1)
        .step_by(2)
        .map(str::to_owned)
        .collect()
}

/// The library crates: the root manifest's `default-members`.
fn library_crates(root: &Path) -> Vec<String> {
    workspace_list(root, "default-members")
}

/// Whether a crate's path names one of the service's crates.
fn is_service(path: &str) -> bool {
    path.rsplit('/')
        .next()
        .is_some_and(|name| name.starts_with("meta-whatsapp-server"))
}

/// Every `.rs` file under `dir`.
fn rust_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let mut entries: Vec<PathBuf> = fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().path())
        .collect();
    entries.sort();
    for path in entries {
        if path.is_dir() {
            rust_files(&path, out);
        } else if path.extension().is_some_and(|e| e == "rs") {
            out.push(path);
        }
    }
}

/// Whether `line` closes a block opened at `indent`: a `}` at that
/// indentation, maybe followed by a comment.
fn closes(line: &str, indent: &str) -> bool {
    line.strip_prefix(indent)
        .and_then(|rest| rest.strip_prefix('}'))
        .is_some_and(|rest| {
            let rest = rest.trim();
            rest.is_empty() || rest.starts_with("//")
        })
}

/// The library lines of `file` (number, text), and the paths of the
/// out-of-line test modules it declares.
///
/// Left out: comment lines, and each inline `#[cfg(test)] mod … { … }`,
/// from its attribute to its closing brace, the first later line that is
/// a `}` at the `mod` line's indentation (where rustfmt, checked by
/// `just lint`, puts it). The scan goes on after that brace: a test module
/// is not always last in its file. Clippy's `items_after_test_module`
/// lets a module or a macro's items follow one, and a nested test module
/// ends before its parent's later items. Only a `#[cfg(test)]` that gates
/// a `mod` counts; one on an `impl`, a `fn` or anything else leaves the
/// rest of the file scanned. A gated module the scan cannot read (no
/// closing brace where rustfmt puts it, an unexpected shape) fails the
/// scan rather than skipping code.
fn library_lines(file: &Path, text: &str) -> (Vec<(usize, String)>, Vec<PathBuf>) {
    let lines: Vec<&str> = text.lines().collect();
    // Where this file's `mod x;` children live.
    let children = match file.file_name().and_then(|n| n.to_str()) {
        Some("mod.rs" | "lib.rs" | "main.rs") => file.parent().unwrap().to_path_buf(),
        _ => file.with_extension(""),
    };
    let (mut code, mut test_modules) = (Vec::new(), Vec::new());
    let mut i = 0;
    while i < lines.len() {
        let raw = lines[i];
        let line = raw.trim();
        if line == "#[cfg(test)]" {
            // The item it gates, past any further attributes.
            let at = (i + 1..lines.len())
                .find(|&j| !lines[j].trim().starts_with("#["))
                .unwrap_or(lines.len());
            let item = lines.get(at).map_or("", |l| l.trim());
            let module = ["mod ", "pub mod ", "pub(crate) mod ", "pub(super) mod "]
                .iter()
                .find_map(|prefix| item.strip_prefix(prefix));
            if let Some(module) = module {
                // `name;` or `name {`, then maybe a comment.
                let module = module.split("//").next().unwrap().trim_end();
                let name_len = module
                    .find(|c: char| !(c.is_alphanumeric() || c == '_'))
                    .unwrap_or(module.len());
                let (name, rest) = module.split_at(name_len);
                match rest.trim() {
                    ";" => {
                        test_modules.push(children.join(format!("{name}.rs")));
                        test_modules.push(children.join(name));
                    }
                    "{}" => {
                        i = at + 1;
                        continue;
                    }
                    "{" => {
                        let indent = &lines[at][..lines[at].len() - lines[at].trim_start().len()];
                        let end = (at + 1..lines.len())
                            .find(|&j| closes(lines[j], indent))
                            .unwrap_or_else(|| {
                                panic!(
                                    "{}:{}: no closing brace for the test module `{name}`",
                                    file.display(),
                                    at + 1
                                )
                            });
                        i = end + 1;
                        continue;
                    }
                    _ => panic!(
                        "{}:{}: a test module the scan cannot read: {item}",
                        file.display(),
                        at + 1
                    ),
                }
            }
        }
        if !line.starts_with("//") {
            code.push((i + 1, raw.to_owned()));
        }
        i += 1;
    }
    (code, test_modules)
}

/// The line numbers [`library_lines`] keeps.
fn kept(file: &str, sample: &str) -> Vec<usize> {
    library_lines(Path::new(file), sample)
        .0
        .iter()
        .map(|(n, _)| *n)
        .collect()
}

/// What [`library_lines`] keeps, on samples: code past an out-of-line
/// `mod tests;` (a comment after it too) and past a gated `impl`, not
/// comments, not an inline test module's lines, and the items after one,
/// at the top level or around a nested one; the declared test files,
/// beside a `mod.rs` and beside any other file.
#[test]
fn the_scan_keeps_library_lines_only() {
    let sample = "fn a() {}\n\
        #[cfg(test)]\n\
        mod tests;\n\
        fn b() {}\n\
        // a comment\n\
        #[cfg(test)]\n\
        impl A {}\n\
        fn c() {}\n\
        #[cfg(test)]\n\
        #[allow(unused)]\n\
        mod inline {\n\
        fn t() {}\n\
        }\n";
    let (code, tests) = library_lines(Path::new("src/m/mod.rs"), sample);
    let kept_lines: Vec<usize> = code.iter().map(|(n, _)| *n).collect();
    assert_eq!(kept_lines, [1, 2, 3, 4, 6, 7, 8]);
    assert_eq!(
        tests,
        [
            Path::new("src/m").join("tests.rs"),
            Path::new("src/m").join("tests")
        ]
    );
    let (_, tests) = library_lines(Path::new("src/m/otp.rs"), "#[cfg(test)]\nmod tests;\n");
    assert_eq!(tests[0], Path::new("src/m/otp").join("tests.rs"));
    let (code, tests) = library_lines(
        Path::new("src/lib.rs"),
        "#[cfg(test)]\npub(crate) mod testing;\nfn d() {}\n#[cfg(test)]\npub mod inline {\n}\nfn e() {}\n",
    );
    assert_eq!(tests[0], Path::new("src").join("testing.rs"));
    assert_eq!(
        code.iter().map(|(n, _)| *n).collect::<Vec<_>>(),
        [1, 2, 3, 7]
    );

    // A comment after `mod tests;` leaves it out of line, and the rest of
    // the file scanned.
    let (code, tests) = library_lines(
        Path::new("src/m/mod.rs"),
        "#[cfg(test)]\nmod tests; // unit tests\nfn f() {}\n",
    );
    assert_eq!(tests[0], Path::new("src/m").join("tests.rs"));
    assert_eq!(code.iter().map(|(n, _)| *n).collect::<Vec<_>>(), [1, 2, 3]);
    // A module after a test module (clippy's `items_after_test_module`
    // allows it) is scanned, and so is an empty test module's neighbour.
    let after = "#[cfg(test)]\n\
        mod tests {\n\
        \x20   fn t() {\n\
        \x20   }\n\
        }\n\
        pub mod later {\n\
        \x20   fn g() {}\n\
        }\n\
        #[cfg(test)]\n\
        mod empty {}\n\
        fn h() {}\n";
    assert_eq!(kept("src/m/mod.rs", after), [6, 7, 8, 11]);
    // A nested test module ends at its own brace, not at the file's end.
    let nested = "mod outer {\n\
        \x20   fn i() {}\n\
        \x20   #[cfg(test)]\n\
        \x20   mod tests {\n\
        \x20       fn t() {}\n\
        \x20   } // mod tests\n\
        \x20   fn j() {}\n\
        }\n\
        fn k() {}\n";
    assert_eq!(kept("src/m/mod.rs", nested), [1, 2, 7, 8, 9]);
}

/// A gated module the scan cannot read fails it, rather than skipping the
/// rest of the file.
#[test]
#[should_panic(expected = "no closing brace for the test module `tests`")]
fn a_test_module_without_its_closing_brace_fails_the_scan() {
    library_lines(
        Path::new("src/m/mod.rs"),
        "#[cfg(test)]\nmod tests {\n    fn t() {}\n    }\nfn f() {}\n",
    );
}

#[test]
fn nothing_in_the_library_calls_the_contact_book_deletion() {
    let root = repo();
    let crates = library_crates(&root);
    assert!(
        crates.iter().any(|c| c == "crates/meta-whatsapp-client")
            && crates.iter().any(|c| c == "crates/meta-whatsapp-rs"),
        "the library crates: {crates:?}"
    );
    // The library is every crate but the service's: a new crate, a member
    // of the workspace or a directory under `crates/`, is one or the other,
    // and the scan never loses one silently.
    let mut candidates = workspace_list(&root, "members");
    for dir in fs::read_dir(root.join("crates")).unwrap() {
        let dir = dir.unwrap().path();
        if dir.is_dir() {
            candidates.push(rel(&dir));
        }
    }
    for name in &candidates {
        assert!(
            is_service(name) != crates.contains(name),
            "{name}: a library crate is a default member, a service crate \
             (`meta-whatsapp-server*`) is not ({crates:?})"
        );
    }

    let mut files = Vec::new();
    for krate in &crates {
        rust_files(&root.join(krate).join("src"), &mut files);
    }
    assert!(files.len() > 50, "walked the library's sources");
    let mut test_modules = Vec::new();
    let mut scanned = Vec::new();
    for file in &files {
        let (code, tests) = library_lines(file, &fs::read_to_string(file).unwrap());
        test_modules.extend(tests);
        scanned.push((file, code));
    }
    scanned.retain(|(file, _)| !test_modules.iter().any(|t| file.starts_with(t)));

    let mut own = Vec::new();
    let mut others = Vec::new();
    for (file, code) in &scanned {
        let path = rel(file);
        for (number, line) in code {
            if !line.contains("contact_book") {
                continue;
            }
            let hit = format!("{path}:{number}: {}", line.trim());
            if path == OWN_FILE && (line.contains(OWN_SIGNATURE) || line.contains(OWN_PATH_SEGMENT))
            {
                own.push(hit);
            } else if !ALLOWED_CALLERS.contains(&path.as_str()) {
                others.push(hit);
            }
        }
    }
    assert_eq!(
        own.len(),
        2,
        "the method's signature and its path segment, once each: {own:#?}"
    );
    assert!(
        others.is_empty(),
        "only `PhoneNumber::delete_contact_book_entry` names the contact book \
         in the library; a caller needs its file in ALLOWED_CALLERS, with the \
         reason: {others:#?}"
    );
}
