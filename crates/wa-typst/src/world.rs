//! The sandboxed [`typst::World`] every render compiles in.
//!
//! One world per render: it owns the template source and a standard library
//! whose `sys.inputs` carries that render's JSON. Fonts are the only shared
//! state, loaded once per process.
//!
//! The world has no file system, no network and no clock:
//!
//! - `source`/`file` answer only for the template itself. Any other path is
//!   refused, and package imports (`@preview/...`) fail with a message saying
//!   packages are disabled, rather than typst's generic "not found" that would
//!   send someone looking for a missing download.
//! - `today` returns the date the caller configured, or `None` (typst then
//!   reports an error), never the system clock: the same input must give the
//!   same bytes on every machine and every day.

use std::sync::LazyLock;

use typst::diag::{FileError, FileResult, PackageError};
use typst::ecow::eco_format;
use typst::foundations::{Bytes, Datetime, Dict, Duration, Str, Value};
use typst::syntax::{FileId, Source, VirtualRoot};
use typst::text::{Font, FontBook};
use typst::utils::LazyHash;
use typst::{Library, LibraryExt as _, World};

/// Fonts bundled by `typst-assets` (families `"Libertinus Serif"`,
/// `"New Computer Modern"`, `"New Computer Modern Math"`,
/// `"DejaVu Sans Mono"`), parsed once per process.
///
/// Never system fonts: output must be byte-identical in CI, the devcontainer
/// and production.
struct Fonts {
    book: LazyHash<FontBook>,
    fonts: Vec<Font>,
}

static FONTS: LazyLock<Fonts> = LazyLock::new(|| {
    let fonts: Vec<Font> = typst_assets::fonts()
        .flat_map(|data| Font::iter(Bytes::new(data)))
        .collect();
    Fonts {
        book: LazyHash::new(FontBook::from_fonts(&fonts)),
        fonts,
    }
});

/// A single-file, sandboxed world.
pub(crate) struct RenderWorld {
    library: LazyHash<Library>,
    main: Source,
    today: Option<Datetime>,
}

impl RenderWorld {
    /// A world compiling `source` with `sys.inputs.data` set to `input_json`.
    ///
    /// The main file is always `/main.typ`, whatever the template is called:
    /// typst interns file ids process-wide with a hard cap of 2^16 and panics
    /// past it, so deriving the path from a caller-chosen name would let a
    /// long-running service exhaust the table. Worlds with the same id but
    /// different text are safe; typst's memoization validates on content.
    pub(crate) fn new(source: &str, input_json: String, today: Option<time::Date>) -> Self {
        let mut inputs = Dict::new();
        inputs.insert(Str::from("data"), Value::Str(Str::from(input_json)));
        Self {
            library: LazyHash::new(Library::builder().with_inputs(inputs).build()),
            main: Source::detached(source),
            today: today.map(Datetime::Date),
        }
    }

    /// The template source, for resolving diagnostic positions.
    pub(crate) fn main_source(&self) -> &Source {
        &self.main
    }

    /// Why any file other than the template is unavailable.
    fn refuse(id: FileId) -> FileError {
        match id.root() {
            VirtualRoot::Package(spec) => {
                FileError::Package(PackageError::Other(Some(eco_format!(
                    "package imports are disabled in wa-typst, so {spec} cannot be loaded; \
                     templates must be self-contained"
                ))))
            }
            VirtualRoot::Project => FileError::Other(Some(eco_format!(
                "file access is disabled in wa-typst, so {} cannot be read; \
                 pass data through `sys.inputs.data`",
                id.vpath().get_with_slash()
            ))),
        }
    }
}

impl World for RenderWorld {
    fn library(&self) -> &LazyHash<Library> {
        &self.library
    }

    fn book(&self) -> &LazyHash<FontBook> {
        &FONTS.book
    }

    fn main(&self) -> FileId {
        self.main.id()
    }

    fn source(&self, id: FileId) -> FileResult<Source> {
        if id == self.main.id() {
            Ok(self.main.clone())
        } else {
            Err(Self::refuse(id))
        }
    }

    fn file(&self, id: FileId) -> FileResult<Bytes> {
        if id == self.main.id() {
            Ok(Bytes::from_string(self.main.text().to_owned()))
        } else {
            Err(Self::refuse(id))
        }
    }

    fn font(&self, index: usize) -> Option<Font> {
        FONTS.fonts.get(index).cloned()
    }

    /// The configured date, for any `offset`: a document dated by the caller
    /// shows that date, not one shifted by the server's time zone.
    fn today(&self, _offset: Option<Duration>) -> Option<Datetime> {
        self.today
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bundled_fonts_are_loaded() {
        // The `fonts` feature of typst-assets is what makes this non-empty;
        // without it every render would come out with no glyphs.
        assert!(FONTS.fonts.len() >= 10, "only {} fonts", FONTS.fonts.len());
        // The book keys families in lower case.
        for family in [
            "libertinus serif",
            "new computer modern",
            "new computer modern math",
            "dejavu sans mono",
        ] {
            assert!(FONTS.book.contains_family(family), "{family} missing");
        }
    }

    #[test]
    fn only_the_template_is_readable() {
        let world = RenderWorld::new("hello", "{}".to_owned(), None);
        assert_eq!(
            world.source(world.main()).map(|s| s.text().to_owned()),
            Ok("hello".into())
        );
        assert_eq!(
            world.file(world.main()).map(|b| b.to_vec()),
            Ok(b"hello".to_vec())
        );

        let other = world.main().map(|p| p.with_extension("json")).intern();
        let err = world
            .file(other)
            .err()
            .map(|e| e.to_string())
            .unwrap_or_default();
        assert!(err.contains("file access is disabled"), "{err}");
    }

    #[test]
    fn today_is_the_configured_date_whatever_the_offset() {
        let date = time::macros::date!(2026 - 09 - 24);
        let world = RenderWorld::new("", "{}".to_owned(), Some(date));
        assert_eq!(world.today(None), Some(Datetime::Date(date)));
        assert_eq!(
            world.today(Some(Duration::from(time::Duration::hours(-11)))),
            Some(Datetime::Date(date))
        );
        assert_eq!(
            RenderWorld::new("", "{}".to_owned(), None).today(None),
            None
        );
    }
}
