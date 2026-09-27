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
//!   M5c3.
//! - **What it skips**: comment lines, inline `#[cfg(test)]` modules and
//!   the files of out-of-line ones ([`library_lines`]). Everything else
//!   after a `#[cfg(test)]` is scanned (a gated `impl` or `fn`).
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

/// The library crates: the root manifest's `default-members`.
fn library_crates(root: &Path) -> Vec<String> {
    let manifest = fs::read_to_string(root.join("Cargo.toml")).unwrap();
    let start = manifest
        .find("default-members = [")
        .expect("the root manifest lists its default members");
    let list = &manifest[start..];
    let list = &list[..list.find(']').unwrap()];
    list.split('"')
        .skip(1)
        .step_by(2)
        .map(str::to_owned)
        .collect()
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

/// The library lines of `file` (number, text), and the paths of the
/// out-of-line test modules it declares.
///
/// Left out: comment lines, and everything from an inline
/// `#[cfg(test)] mod … {` on. A test module is last in its file: clippy's
/// `items_after_test_module`, denied by `just lint`, refuses an item
/// after it. Only a `#[cfg(test)]` that gates a `mod` counts; one on an
/// `impl`, a `fn` or anything else leaves the rest of the file scanned.
fn library_lines(file: &Path, text: &str) -> (Vec<(usize, String)>, Vec<PathBuf>) {
    let lines: Vec<&str> = text.lines().collect();
    // Where this file's `mod x;` children live.
    let children = match file.file_name().and_then(|n| n.to_str()) {
        Some("mod.rs" | "lib.rs" | "main.rs") => file.parent().unwrap().to_path_buf(),
        _ => file.with_extension(""),
    };
    let (mut code, mut test_modules) = (Vec::new(), Vec::new());
    for (i, raw) in lines.iter().enumerate() {
        let line = raw.trim();
        if line == "#[cfg(test)]" {
            // The item it gates, past any further attributes.
            let item = lines[i + 1..]
                .iter()
                .map(|l| l.trim())
                .find(|l| !l.starts_with("#["))
                .unwrap_or_default();
            let module = ["mod ", "pub mod ", "pub(crate) mod ", "pub(super) mod "]
                .iter()
                .find_map(|prefix| item.strip_prefix(prefix));
            match module.map(|m| m.strip_suffix(';')) {
                Some(Some(name)) => {
                    test_modules.push(children.join(format!("{name}.rs")));
                    test_modules.push(children.join(name));
                }
                Some(None) => break,
                None => {}
            }
        }
        if !line.starts_with("//") {
            code.push((i + 1, (*raw).to_owned()));
        }
    }
    (code, test_modules)
}

/// What [`library_lines`] keeps, on a sample: code past an out-of-line
/// `mod tests;` and past a gated `impl`, not comments, and nothing from an
/// inline test module on; the declared test files, beside a `mod.rs` and
/// beside any other file.
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
    let kept: Vec<usize> = code.iter().map(|(n, _)| *n).collect();
    assert_eq!(kept, [1, 2, 3, 4, 6, 7, 8]);
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
    assert_eq!(code.iter().map(|(n, _)| *n).collect::<Vec<_>>(), [1, 2, 3]);
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
    // The library is every crate but the service's: a new crate is one or
    // the other, and the scan never loses one silently.
    for dir in fs::read_dir(root.join("crates")).unwrap() {
        let dir = dir.unwrap().path();
        if !dir.is_dir() {
            continue;
        }
        let name = rel(&dir);
        let service = dir
            .file_name()
            .and_then(|n| n.to_str())
            .is_some_and(|n| n.starts_with("meta-whatsapp-server"));
        assert!(
            service != crates.contains(&name),
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
