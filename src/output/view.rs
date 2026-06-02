//! Terminal markdown renderer for `fastio view`.
//!
//! Renders a markdown document (a note's `content` or a raw `.md` file) for a
//! human reading it in a terminal. Rendering is **TTY-gated**: when stdout is
//! not a terminal (piped/redirected), or the caller requests `--raw` /
//! `--no-color`, the raw markdown is written through verbatim so scripts and
//! pipelines get byte-faithful output and an LLM consumer is not handed ANSI
//! escape sequences.
//!
//! Safety: the renderer is `unwrap()`/`expect()`/`panic!()`-free on hostile or
//! malformed markdown — `termimad` parses leniently and never panics on bad
//! input, and any write failure surfaces as an [`std::io::Error`]. It launches
//! **no pager** and spawns no child process, so it is safe inside scripts.
//!
//! User-controlled text in the raw path is passed through unchanged (the
//! markdown body is the payload the user asked to see); the structured
//! bucket/markdown envelope renderers in [`super`] remain the place where
//! arbitrary API fields are sanitized via the `push_sanitized_*` helpers.

use std::io::{self, IsTerminal, Write};

use termimad::MadSkin;

/// How `fastio view` should present the markdown body.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum ViewMode {
    /// Pretty terminal rendering (colors + layout) — only when stdout is a TTY.
    Rendered,
    /// Verbatim markdown passthrough (no ANSI), for `--raw`, `--no-color`, or
    /// non-TTY stdout.
    Raw,
}

impl ViewMode {
    /// Decide the effective view mode from the caller's flags and the runtime
    /// TTY state.
    ///
    /// Returns [`ViewMode::Raw`] when any of: the user passed `--raw`, the user
    /// passed `--no-color`, or stdout is not a terminal. Otherwise
    /// [`ViewMode::Rendered`]. The TTY decision is taken from the supplied
    /// `stdout_is_tty` so callers can unit-test both branches.
    #[must_use]
    pub fn resolve(raw: bool, no_color: bool, stdout_is_tty: bool) -> Self {
        if raw || no_color || !stdout_is_tty {
            Self::Raw
        } else {
            Self::Rendered
        }
    }

    /// Resolve using the real process stdout TTY state.
    #[must_use]
    pub fn resolve_runtime(raw: bool, no_color: bool) -> Self {
        Self::resolve(raw, no_color, io::stdout().is_terminal())
    }
}

/// Render `markdown` to stdout in the given `mode`.
///
/// In [`ViewMode::Raw`], the markdown is written byte-for-byte verbatim — no
/// trailing newline is injected, so scripts and pipelines receive exactly the
/// bytes the server sent. In [`ViewMode::Rendered`], `termimad` styles it for
/// the terminal (and a single trailing newline is appended if missing for
/// cosmetic reasons). Never launches a pager.
///
/// # Errors
///
/// Returns an [`io::Error`] if writing to stdout fails (e.g. broken pipe).
pub fn render_markdown(markdown: &str, mode: ViewMode) -> io::Result<()> {
    let mut stdout = io::stdout().lock();
    render_markdown_to(&mut stdout, markdown, mode)
}

/// Render `markdown` into an arbitrary writer in the given `mode`.
///
/// The writer-generic core of [`render_markdown`], factored out so tests can
/// capture into an in-memory buffer (and so the hostile-input safety tests do
/// not spray ANSI onto the test harness's stdout). Never launches a pager and
/// spawns no child process; `termimad` never panics on malformed markdown.
///
/// # Errors
///
/// Returns an [`io::Error`] if writing to `w` fails.
pub fn render_markdown_to<W: Write>(w: &mut W, markdown: &str, mode: ViewMode) -> io::Result<()> {
    match mode {
        ViewMode::Raw => {
            // Byte-faithful passthrough: write exactly the input bytes and
            // return. `--raw` / non-TTY callers (scripts, pipelines) rely on
            // verbatim output, so no trailing newline is injected and no
            // transformation is applied.
            w.write_all(markdown.as_bytes())
        }
        ViewMode::Rendered => {
            // `MadSkin::default()` selects sensible terminal styling.
            // `term_text` builds an in-memory styled representation that is
            // `Display`-able; we format it to a string and write it.
            let skin = MadSkin::default();
            let body = format!("{}", skin.term_text(markdown));
            w.write_all(body.as_bytes())?;
            // Cosmetic: a human reading rendered output wants a final newline;
            // this is intentionally NOT applied to the Raw byte-faithful path.
            if !body.ends_with('\n') {
                w.write_all(b"\n")?;
            }
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolve_raw_when_raw_flag() {
        assert_eq!(ViewMode::resolve(true, false, true), ViewMode::Raw);
    }

    #[test]
    fn resolve_raw_when_no_color() {
        assert_eq!(ViewMode::resolve(false, true, true), ViewMode::Raw);
    }

    #[test]
    fn resolve_raw_when_not_tty() {
        assert_eq!(ViewMode::resolve(false, false, false), ViewMode::Raw);
    }

    #[test]
    fn resolve_rendered_when_tty_and_no_flags() {
        assert_eq!(ViewMode::resolve(false, false, true), ViewMode::Rendered);
    }

    #[test]
    fn render_raw_writes_bytes_exactly_without_appending_newline() {
        // Raw mode promises byte-faithful passthrough: input without a trailing
        // newline must NOT gain one.
        let input = "# Title\n\nBody";
        let mut buf = Vec::new();
        render_markdown_to(&mut buf, input, ViewMode::Raw).unwrap();
        assert_eq!(buf, input.as_bytes());
    }

    #[test]
    fn render_raw_preserves_existing_trailing_newline_exactly() {
        // A trailing newline that IS present is preserved (not doubled).
        let input = "# Title\n\nBody\n";
        let mut buf = Vec::new();
        render_markdown_to(&mut buf, input, ViewMode::Raw).unwrap();
        assert_eq!(buf, input.as_bytes());
    }

    #[test]
    fn render_raw_empty_input_writes_nothing() {
        let mut buf = Vec::new();
        render_markdown_to(&mut buf, "", ViewMode::Raw).unwrap();
        assert!(buf.is_empty());
    }

    #[test]
    fn render_raw_does_not_panic_on_hostile_markdown() {
        // Unbalanced fences, control chars, bidi overrides — must not panic.
        let hostile = "```rust\nunclosed\n\u{202E}\u{0007}# heading\n| a | b\n|---";
        let mut buf = Vec::new();
        assert!(render_markdown_to(&mut buf, hostile, ViewMode::Raw).is_ok());
    }

    #[test]
    fn render_rendered_does_not_panic_on_hostile_markdown() {
        // Rendered mode exercises termimad's parser; must not panic on
        // unbalanced fences, broken tables, control chars, or bidi overrides.
        let hostile = "```\n| broken | table\n\u{202E}\u{0000}**bold";
        let mut buf = Vec::new();
        assert!(render_markdown_to(&mut buf, hostile, ViewMode::Rendered).is_ok());
        assert!(!buf.is_empty());
    }
}
