//! Minimal `typst::World` implementation for rendering embedded templates.

use std::collections::HashMap;
use std::sync::Arc;
use typst::diag::FileResult;
use typst::foundations::Bytes;
use typst::syntax::{FileId, RootedPath, Source, VirtualPath, VirtualRoot};
use typst::text::{Font, FontBook};
use typst::utils::LazyHash;
use typst::{Library, World};

/// A minimal World implementation for embedded templates.
pub(crate) struct WaWorld {
    /// The main source file id.
    main_id: FileId,
    /// Main source file being compiled.
    #[allow(dead_code)]
    main_source: Source,
    /// All available sources (just the main one for embedded templates).
    sources: HashMap<FileId, Source>,
    /// Compiled library.
    library: Arc<LazyHash<Library>>,
    /// Font book.
    font_book: Arc<LazyHash<FontBook>>,
    /// Fonts.
    fonts: HashMap<usize, Arc<Font>>,
}

impl WaWorld {
    /// Create a new world for rendering a template.
    ///
    /// # Panics
    ///
    /// Panics if the template name cannot be converted to a valid `VirtualPath`.
    /// This should only happen with extremely malformed input.
    #[allow(clippy::expect_used)]
    pub(crate) fn new(
        name: impl Into<String>,
        source: &str,
        library: Arc<LazyHash<Library>>,
        font_book: Arc<LazyHash<FontBook>>,
        fonts: Vec<Font>,
    ) -> Self {
        let name_str = name.into();

        // Create a FileId for the main source
        // Use Project root and the template name as the path
        let vpath = VirtualPath::new(&name_str)
            .or_else(|_| VirtualPath::new("template.typ"))
            .or_else(|_| VirtualPath::new("t.typ"))
            .expect("at least one virtual path should be valid");

        let rooted = RootedPath::new(VirtualRoot::Project, vpath);
        let main_id = rooted.intern();

        let main_source = Source::new(main_id, source.to_string());

        let mut sources = HashMap::new();
        sources.insert(main_id, main_source.clone());

        let fonts_map: HashMap<usize, Arc<Font>> = fonts
            .into_iter()
            .enumerate()
            .map(|(i, f)| (i, Arc::new(f)))
            .collect();

        Self {
            main_id,
            main_source,
            sources,
            library,
            font_book,
            fonts: fonts_map,
        }
    }
}

impl World for WaWorld {
    fn library(&self) -> &LazyHash<Library> {
        &self.library
    }

    fn book(&self) -> &LazyHash<FontBook> {
        &self.font_book
    }

    fn main(&self) -> FileId {
        self.main_id
    }

    fn source(&self, id: FileId) -> FileResult<Source> {
        self.sources.get(&id).cloned().ok_or_else(|| {
            // Create a simple path string for the not found error
            typst::diag::FileError::NotFound("template".into())
        })
    }

    fn file(&self, id: FileId) -> FileResult<Bytes> {
        self.source(id)
            .map(|src| Bytes::new(src.text().to_string()))
    }

    fn font(&self, index: usize) -> Option<Font> {
        self.fonts.get(&index).map(|f| (**f).clone())
    }

    fn today(
        &self,
        _offset: Option<typst::foundations::Duration>,
    ) -> Option<typst::foundations::Datetime> {
        None
    }
}
