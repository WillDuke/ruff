//! Writes the HTML coverage report for `ty check --html-report`.
//!
//! The report follows the layout of mypy's `--html-report`: an `index.html` page with one row per
//! file, and one page per file that shows its source with each line colored by its
//! [`LineCoverage`]. The pages are static HTML with an embedded stylesheet, so each page works on
//! its own.

use std::fmt::{self, Display, Formatter};
use std::io;

use anyhow::Context;
use camino::Utf8Component;
use rayon::iter::{IntoParallelRefIterator, ParallelIterator};
use ruff_db::cancellation::CancellationToken;
use ruff_db::files::File;
use ruff_db::source::{SourceText, is_notebook, source_text};
use ruff_db::system::{SystemPath, SystemPathBuf, WritableSystem};
use ruff_source_file::UniversalNewlines;
use rustc_hash::FxHashSet;
use ty_python_semantic::coverage::{Line, LineCoverage, line_coverage};

use crate::parallel::ParallelIteratorExt;
use crate::{Db, ProjectFiles};

/// Line counts for a file, or for all files in the report.
#[derive(Debug, Default, Clone, Copy)]
pub struct CoverageCounts {
    empty: usize,
    precise: usize,
    imprecise: usize,
    dynamic: usize,
}

impl CoverageCounts {
    fn add(&mut self, coverage: LineCoverage) {
        match coverage {
            LineCoverage::Empty => self.empty += 1,
            LineCoverage::Precise => self.precise += 1,
            LineCoverage::Imprecise => self.imprecise += 1,
            LineCoverage::Dynamic => self.dynamic += 1,
        }
    }

    fn merge(&mut self, other: CoverageCounts) {
        self.empty += other.empty;
        self.precise += other.precise;
        self.imprecise += other.imprecise;
        self.dynamic += other.dynamic;
    }

    fn total(self) -> usize {
        self.empty + self.precise + self.imprecise + self.dynamic
    }

    /// The share of lines that are imprecise or dynamic, as in mypy's report.
    #[expect(clippy::cast_precision_loss)]
    pub fn imprecision(self) -> f64 {
        let total = self.total();
        if total == 0 {
            0.0
        } else {
            (self.imprecise + self.dynamic) as f64 / total as f64
        }
    }
}

/// Writes the report for the project's checked files into `output_dir`.
///
/// Returns the line counts over all files, or `None` if `cancellation_token` was cancelled while
/// the report was being written. This runs after `ty check` has inferred the types of the
/// project's files, so it mostly reads cached results.
///
/// Notebooks are left out: their source is the text of all their cells, which doesn't read as a
/// page of code.
pub fn write_html_report(
    db: &dyn Db,
    output_dir: &SystemPath,
    cancellation_token: &CancellationToken,
) -> anyhow::Result<Option<CoverageCounts>> {
    let project = db.project();
    let project_files = ProjectFiles::new(db, project);
    let mut files: Vec<(File, SystemPathBuf)> = (&project_files)
        .into_iter()
        .filter(|&file| !is_notebook(db, file))
        .filter_map(|file| Some((file, file.path(db).as_system_path()?.to_path_buf())))
        .collect();
    files.sort_by(|(_, a), (_, b)| a.cmp(b));

    // Show paths relative to the deepest directory that contains all files.
    let prefix = common_parent(files.iter().map(|(_, path)| path.as_path()));
    let relative_path = |path: &SystemPathBuf| report_path(path, &prefix);

    // Write the file pages in parallel, keeping only their line counts so that the coverage of all
    // files is never in memory at once.
    let counts: Vec<Option<CoverageCounts>> = files
        .par_iter()
        .map_with_db(db, |db, (file, path)| {
            if cancellation_token.is_cancelled() {
                return Ok(None);
            }
            let page = FilePage::new(db, *file, relative_path(path));
            write_file(db, &output_dir.join(page.page_path()), &page)?;
            Ok(Some(page.counts))
        })
        .collect::<anyhow::Result<_>>()?;
    let Some(counts) = counts.into_iter().collect::<Option<Vec<_>>>() else {
        return Ok(None);
    };

    let index = IndexPage {
        project: project.name(db),
        files: files
            .iter()
            .zip(counts)
            .map(|((_, path), counts)| (relative_path(path), counts))
            .collect(),
    };
    write_file(db, &output_dir.join("index.html"), &index)?;

    let pages: FxHashSet<SystemPathBuf> = index
        .files
        .iter()
        .map(|(path, _)| output_dir.join(page_path(path)))
        .collect();
    remove_stale_pages(writable_system(db)?, &output_dir.join("files"), &pages)?;

    Ok(Some(index.total()))
}

/// The index page: a summary of the project, and a row per file.
struct IndexPage<'a> {
    project: &'a str,
    /// Each file's path relative to the report's root (see [`report_path`]), and its line counts.
    files: Vec<(String, CoverageCounts)>,
}

impl IndexPage<'_> {
    fn total(&self) -> CoverageCounts {
        let mut total = CoverageCounts::default();
        for (_, counts) in &self.files {
            total.merge(*counts);
        }
        total
    }
}

impl Display for IndexPage<'_> {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        let project = Escaped(self.project);
        let total = self.total();
        writeln!(f, "<!DOCTYPE html>\n<html lang=\"en\">\n<head>")?;
        write_head(f, format_args!("{project} type coverage"))?;
        writeln!(f, "<body>\n<h1>{project} type coverage</h1>")?;
        writeln!(
            f,
            "<p class=\"subtitle\">{:.2}% of {} lines in {} files are imprecise or dynamic</p>",
            total.imprecision() * 100.0,
            total.total(),
            self.files.len()
        )?;
        writeln!(f, "{}", Bar(total, "bar total"))?;
        writeln!(
            f,
            "<div class=\"table-wrapper\">\n<table class=\"summary\">\n<thead><tr><th>File</th><th></th><th>Imprecision</th><th>Lines</th><th>Precise</th><th>Imprecise</th><th>Dynamic</th><th>Empty</th></tr></thead>\n<tbody>"
        )?;
        for (path, counts) in &self.files {
            writeln!(
                f,
                "<tr><td><a href=\"{}\">{}</a></td><td>{}</td><td>{:.2}%</td><td>{}</td><td>{}</td><td>{}</td><td>{}</td><td>{}</td></tr>",
                Escaped(&page_path(path)),
                Escaped(path),
                Bar(*counts, "bar"),
                counts.imprecision() * 100.0,
                counts.total(),
                counts.precise,
                counts.imprecise,
                counts.dynamic,
                counts.empty,
            )?;
        }
        writeln!(f, "</tbody>\n</table>\n</div>\n</body>\n</html>")
    }
}

/// The page for a single file: its source, with each line colored by its coverage.
struct FilePage {
    /// The file's path relative to the report's root (see [`report_path`]).
    relative_path: String,
    source: SourceText,
    lines: Vec<Line>,
    counts: CoverageCounts,
}

impl FilePage {
    fn new(db: &dyn Db, file: File, relative_path: String) -> Self {
        let source = source_text(db, file);
        let lines = line_coverage(db, file);

        // Split lines as the classification's line index does, so that they line up, including
        // with `\r` line endings.
        let mut counts = CoverageCounts::default();
        for index in 0..source.as_str().universal_newlines().count() {
            counts.add(
                lines
                    .get(index)
                    .map_or(LineCoverage::Empty, |line| line.coverage),
            );
        }

        Self {
            relative_path,
            source,
            lines,
            counts,
        }
    }

    /// The path of this page, relative to the report's root.
    fn page_path(&self) -> String {
        page_path(&self.relative_path)
    }
}

impl Display for FilePage {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        let title = Escaped(&self.relative_path);
        // Link back to `index.html` from the page's directory. Page paths always use `/` as their
        // separator (see `report_path`), so this also holds on Windows.
        let index_link = "../".repeat(self.page_path().matches('/').count()) + "index.html";

        writeln!(f, "<!DOCTYPE html>\n<html lang=\"en\">\n<head>")?;
        write_head(f, &title)?;
        writeln!(
            f,
            "<body>\n<header>\n<a href=\"{index_link}\">All files</a>"
        )?;
        writeln!(f, "<h1>{title}</h1>")?;
        writeln!(
            f,
            "<span class=\"subtitle\">{} lines, {:.2}% imprecise or dynamic</span>",
            self.counts.total(),
            self.counts.imprecision() * 100.0
        )?;
        writeln!(
            f,
            "<span class=\"legend\"><span class=\"precise\">precise</span><span class=\"imprecise\">imprecise</span><span class=\"dynamic\">dynamic</span></span>\n</header>"
        )?;
        writeln!(f, "<div class=\"source\"><div class=\"lines\">")?;

        for (index, text) in self.source.as_str().universal_newlines().enumerate() {
            let number = index + 1;
            let (coverage, notes) = self
                .lines
                .get(index)
                .map_or((LineCoverage::Empty, &[][..]), |line| {
                    (line.coverage, &line.notes[..])
                });
            let class = match coverage {
                LineCoverage::Empty => "empty",
                LineCoverage::Precise => "precise",
                LineCoverage::Imprecise => "imprecise",
                LineCoverage::Dynamic => "dynamic",
            };
            write!(f, "<div class=\"line {class}\" id=\"L{number}\"")?;
            if !notes.is_empty() {
                write!(f, " title=\"{}\"", Escaped(&notes.join("\n")))?;
            }
            writeln!(
                f,
                "><a href=\"#L{number}\">{number}</a><code>{}</code></div>",
                Escaped(text.as_str())
            )?;
        }

        writeln!(f, "</div></div>\n</body>\n</html>")
    }
}

/// A bar that shows the share of precise, imprecise, and dynamic lines. The rest of the bar, the
/// share of empty lines, shows the bar's background.
struct Bar(CoverageCounts, &'static str);

impl Display for Bar {
    #[expect(clippy::cast_precision_loss)]
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        let Bar(counts, class) = self;
        let total = counts.total().max(1) as f64;
        write!(f, "<div class=\"{class}\">")?;
        for (count, segment) in [
            (counts.precise, "precise"),
            (counts.imprecise, "imprecise"),
            (counts.dynamic, "dynamic"),
        ] {
            let width = count as f64 / total * 100.0;
            write!(
                f,
                "<span class=\"{segment}\" style=\"width: {width:.2}%\"></span>"
            )?;
        }
        write!(f, "</div>")
    }
}

/// Text escaped for use in HTML content and attribute values.
struct Escaped<'a>(&'a str);

impl Display for Escaped<'_> {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        let mut rest = self.0;
        while let Some(index) = rest.find(['&', '<', '>', '"']) {
            f.write_str(&rest[..index])?;
            f.write_str(match rest.as_bytes()[index] {
                b'&' => "&amp;",
                b'<' => "&lt;",
                b'>' => "&gt;",
                _ => "&quot;",
            })?;
            rest = &rest[index + 1..];
        }
        f.write_str(rest)
    }
}

/// Writes the `<head>` contents and closing tag shared by all pages.
fn write_head(f: &mut Formatter<'_>, title: impl Display) -> fmt::Result {
    writeln!(f, "<meta charset=\"utf-8\">")?;
    writeln!(
        f,
        "<meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">"
    )?;
    writeln!(f, "<title>{title}</title>")?;
    writeln!(f, "<style>{STYLE}</style>\n</head>")
}

/// The path of a file's page, relative to the report's root.
fn page_path(relative_path: &str) -> String {
    format!("files/{relative_path}.html")
}

/// Returns `path` relative to `prefix`, with its components separated by `/` on every platform,
/// so that it can be used both in links and as a path below the report's `files` directory.
///
/// Only normal components are kept, so the result never escapes that directory: if `path` isn't
/// below `prefix`, as when the files are on different Windows drives and have no common parent,
/// its root is dropped and a drive prefix such as `C:` becomes a directory `C`.
fn report_path(path: &SystemPath, prefix: &SystemPath) -> String {
    let relative = path.strip_prefix(prefix).unwrap_or(path);
    relative
        .components()
        .filter_map(|component| match component {
            Utf8Component::Normal(name) => Some(name),
            Utf8Component::Prefix(drive) => Some(drive.as_str().trim_end_matches(':')),
            Utf8Component::RootDir | Utf8Component::CurDir | Utf8Component::ParentDir => None,
        })
        .collect::<Vec<_>>()
        .join("/")
}

/// Removes the pages below `directory` that aren't in `pages`, such as the pages of files that
/// were deleted since the report was last written, so that the report doesn't keep outdated
/// pages around.
///
/// Only `.html` files are removed, so that other files in the report's directory are left alone.
fn remove_stale_pages(
    system: &dyn WritableSystem,
    directory: &SystemPath,
    pages: &FxHashSet<SystemPathBuf>,
) -> anyhow::Result<()> {
    let entries = match system.read_directory(directory) {
        Ok(entries) => entries,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(error) => {
            return Err(error).with_context(|| format!("Failed to read directory `{directory}`"));
        }
    };
    let entries = entries
        .collect::<io::Result<Vec<_>>>()
        .with_context(|| format!("Failed to read directory `{directory}`"))?;

    for entry in entries {
        let path = entry.path();
        if entry.file_type().is_directory() {
            remove_stale_pages(system, path, pages)?;
        } else if entry.file_type().is_file()
            && path.extension() == Some("html")
            && !pages.contains(path)
        {
            system
                .remove_file(path)
                .with_context(|| format!("Failed to remove `{path}`"))?;
        }
    }
    Ok(())
}

fn writable_system(db: &dyn Db) -> anyhow::Result<&dyn WritableSystem> {
    db.system()
        .as_writable()
        .context("The active file system is read-only")
}

fn write_file(db: &dyn Db, path: &SystemPath, contents: &impl Display) -> anyhow::Result<()> {
    let system = writable_system(db)?;
    if let Some(parent) = path.parent() {
        system
            .create_directory_all(parent)
            .with_context(|| format!("Failed to create directory `{parent}`"))?;
    }
    system
        .write_file(path, &contents.to_string())
        .with_context(|| format!("Failed to write `{path}`"))
}

/// Returns the deepest directory that contains all `paths`.
fn common_parent<'a>(mut paths: impl Iterator<Item = &'a SystemPath>) -> SystemPathBuf {
    let Some(mut prefix) = paths.next().and_then(SystemPath::parent) else {
        return SystemPathBuf::new();
    };

    for path in paths {
        while !path.starts_with(prefix) {
            let Some(parent) = prefix.parent() else {
                return SystemPathBuf::new();
            };
            prefix = parent;
        }
    }

    prefix.to_path_buf()
}

/// The report's stylesheet, which is copied into every page so that each page works on its own.
///
/// Like mypy's report, precise lines are green, imprecise lines are yellow, and dynamic lines are
/// a warning color. Unlike mypy, we leave empty lines (comments, docstrings, and blank lines)
/// uncolored, so that green only marks lines whose types ty checked and found precise.
const STYLE: &str = include_str!("coverage/report.css");

#[cfg(test)]
mod tests {
    use ruff_db::Db as _;
    use ruff_db::cancellation::CancellationTokenSource;
    use ruff_db::system::{DbWithWritableSystem as _, SystemPath};

    use crate::ProjectMetadata;
    use crate::db::testing::TestDb;

    use super::{report_path, write_html_report};

    fn project() -> anyhow::Result<TestDb> {
        let mut db = TestDb::new(ProjectMetadata::new("test", "/project".into()));
        db.write_file("/project/pkg/precise.py", "x: int = 1\n")?;
        db.write_file("/project/pkg/dynamic.py", "def f(y):\n    return y\n")?;
        Ok(db)
    }

    fn read(db: &TestDb, path: &str) -> anyhow::Result<String> {
        Ok(db.system().read_to_string(SystemPath::new(path))?)
    }

    #[test]
    fn writes_index_and_file_pages() -> anyhow::Result<()> {
        let db = project()?;
        let source = CancellationTokenSource::new();

        let total = write_html_report(&db, SystemPath::new("/report"), &source.token())?;

        assert!(total.is_some_and(|total| total.imprecision() > 0.0));
        let index = read(&db, "/report/index.html")?;
        assert!(index.contains(r#"<a href="files/dynamic.py.html">dynamic.py</a>"#));
        assert!(index.contains(r#"<a href="files/precise.py.html">precise.py</a>"#));
        let page = read(&db, "/report/files/dynamic.py.html")?;
        assert!(page.contains(r#"<div class="line dynamic" id="L1" title="`y`: Unknown">"#));
        Ok(())
    }

    #[test]
    fn splits_lines_like_the_line_index() -> anyhow::Result<()> {
        let mut db = project()?;
        // A lone `\r` ends a line, both for the classification and on the page.
        db.write_file("/project/pkg/carriage.py", "a = 1\rb = len\n")?;
        let source = CancellationTokenSource::new();

        write_html_report(&db, SystemPath::new("/report"), &source.token())?;

        let page = read(&db, "/report/files/carriage.py.html")?;
        assert!(page.contains(
            r##"<div class="line precise" id="L1"><a href="#L1">1</a><code>a = 1</code></div>"##
        ));
        assert!(page.contains(
            r##"<div class="line precise" id="L2"><a href="#L2">2</a><code>b = len</code></div>"##
        ));
        Ok(())
    }

    #[test]
    fn escapes_source_text() -> anyhow::Result<()> {
        let mut db = project()?;
        db.write_file("/project/pkg/html.py", "tag = \"<b>&\"\n")?;
        let source = CancellationTokenSource::new();

        write_html_report(&db, SystemPath::new("/report"), &source.token())?;

        let page = read(&db, "/report/files/html.py.html")?;
        assert!(page.contains("<code>tag = &quot;&lt;b&gt;&amp;&quot;</code>"));
        Ok(())
    }

    #[test]
    fn stops_when_cancelled() -> anyhow::Result<()> {
        let db = project()?;
        let source = CancellationTokenSource::new();
        source.cancel();

        let total = write_html_report(&db, SystemPath::new("/report"), &source.token())?;

        assert!(total.is_none());
        assert!(
            !db.system()
                .path_exists(SystemPath::new("/report/index.html"))
        );
        Ok(())
    }

    #[test]
    fn links_nested_pages_back_to_the_index() -> anyhow::Result<()> {
        let mut db = project()?;
        db.write_file("/project/pkg/sub/nested.py", "z = 1\n")?;
        let source = CancellationTokenSource::new();

        write_html_report(&db, SystemPath::new("/report"), &source.token())?;

        let index = read(&db, "/report/index.html")?;
        assert!(index.contains(r#"<a href="files/sub/nested.py.html">sub/nested.py</a>"#));
        let page = read(&db, "/report/files/sub/nested.py.html")?;
        assert!(page.contains(r#"<a href="../../index.html">All files</a>"#));
        Ok(())
    }

    #[test]
    fn removes_pages_of_files_that_no_longer_exist() -> anyhow::Result<()> {
        let mut db = project()?;
        db.write_file("/report/files/removed.py.html", "")?;
        db.write_file("/report/files/notes.txt", "")?;
        let source = CancellationTokenSource::new();

        write_html_report(&db, SystemPath::new("/report"), &source.token())?;

        assert!(
            !db.system()
                .path_exists(SystemPath::new("/report/files/removed.py.html"))
        );
        assert!(
            db.system()
                .path_exists(SystemPath::new("/report/files/precise.py.html"))
        );
        // Files that aren't pages are left alone.
        assert!(
            db.system()
                .path_exists(SystemPath::new("/report/files/notes.txt"))
        );
        Ok(())
    }

    #[test]
    fn report_paths_stay_below_the_files_directory() {
        assert_eq!(
            report_path(
                SystemPath::new("/project/pkg/a.py"),
                SystemPath::new("/project")
            ),
            "pkg/a.py"
        );
        // A path that isn't below the prefix loses its root rather than becoming absolute.
        assert_eq!(
            report_path(
                SystemPath::new("/elsewhere/a.py"),
                SystemPath::new("/project")
            ),
            "elsewhere/a.py"
        );
    }

    #[cfg(windows)]
    #[test]
    fn report_paths_use_forward_slashes_and_drive_directories() {
        assert_eq!(
            report_path(
                SystemPath::new(r"C:\project\pkg\a.py"),
                SystemPath::new(r"C:\project")
            ),
            "pkg/a.py"
        );
        // Files on different drives have no common parent.
        assert_eq!(
            report_path(SystemPath::new(r"D:\other\b.py"), SystemPath::new("")),
            "D/other/b.py"
        );
    }
}
