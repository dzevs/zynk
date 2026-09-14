// Modified by the zynk project: this file differs from the upstream version it was derived from.
// See NOTICE ("Modified files (Apache-2.0 provenance)") for the provenance and the license terms.

const CLAUDE_ACTIVITY_GLYPHS: &str = "·✢✳✶✻✽◐◓◑◒";

pub(crate) fn stripped_terminal_title(title: &str) -> Option<String> {
    let title = title.trim();
    if title.is_empty() {
        return None;
    }

    let mut chars = title.char_indices();
    let (_, first) = chars.next()?;
    let after_first = &title[first.len_utf8()..];
    let recognized =
        matches!(first, '\u{2800}'..='\u{28ff}') || CLAUDE_ACTIVITY_GLYPHS.contains(first);
    let stripped = if recognized
        && (after_first.is_empty() || after_first.chars().next().is_some_and(char::is_whitespace))
    {
        after_first.trim()
    } else {
        title
    };

    (!stripped.is_empty()).then(|| stripped.to_string())
}

#[cfg(test)]
mod tests {
    use super::stripped_terminal_title;

    #[test]
    fn strips_one_recognized_leading_activity_glyph() {
        for title in [
            "⠋ task",
            "✳ task",
            "  ⠙   task  ",
            "✢ task",
            "✻ task",
            "◐ task",
            "◓ task",
            "◑ task",
            "◒ task",
        ] {
            assert_eq!(stripped_terminal_title(title).as_deref(), Some("task"));
        }
        assert_eq!(
            stripped_terminal_title("⠋ ⠙ task").as_deref(),
            Some("⠙ task")
        );
    }

    #[test]
    fn preserves_unrecognized_or_unbounded_symbols() {
        for (title, expected) in [
            ("★task", "★task"),
            ("★ production", "★ production"),
            ("✨ task", "✨ task"),
            ("☼ status", "☼ status"),
            ("@ task", "@ task"),
            ("task ⠋ detail", "task ⠋ detail"),
            ("[prod] task", "[prod] task"),
        ] {
            assert_eq!(stripped_terminal_title(title).as_deref(), Some(expected));
        }
    }

    #[test]
    fn preserves_unicode_text_and_elides_empty_results() {
        assert_eq!(
            stripped_terminal_title(" ⠋ 修复🙂标题 ").as_deref(),
            Some("修复🙂标题")
        );
        assert_eq!(stripped_terminal_title("  "), None);
        assert_eq!(stripped_terminal_title("⠋   "), None);
    }

    #[test]
    fn m828c_activity_grammar_has_exact_scalar_and_whitespace_boundaries() {
        for glyph in "·✢✳✶✻✽◐◓◑◒".chars().chain(['\u{2800}', '\u{28ff}']) {
            for whitespace in [" ", "\t", "\n", "\u{00a0}", "\u{2003}"] {
                let title = format!("{whitespace}{glyph}{whitespace}task{whitespace}");
                assert_eq!(
                    stripped_terminal_title(&title).as_deref(),
                    Some("task"),
                    "{title:?}"
                );
            }
            assert_eq!(stripped_terminal_title(&glyph.to_string()), None);
            assert_eq!(stripped_terminal_title(&format!("{glyph}   ")), None);
            let unbounded = format!("{glyph}task");
            assert_eq!(
                stripped_terminal_title(&unbounded).as_deref(),
                Some(unbounded.as_str())
            );
            let infix = format!("task {glyph} detail");
            assert_eq!(
                stripped_terminal_title(&infix).as_deref(),
                Some(infix.as_str())
            );
            let twice = format!("{glyph} {glyph} task");
            let once = format!("{glyph} task");
            assert_eq!(
                stripped_terminal_title(&twice).as_deref(),
                Some(once.as_str())
            );
        }
        for glyph in ['\u{27ff}', '\u{2900}', '\u{2605}', '\u{2728}'] {
            let title = format!("{glyph} task");
            assert_eq!(
                stripped_terminal_title(&title).as_deref(),
                Some(title.as_str())
            );
        }
        for (title, expected) in [
            (" \tplain \u{2003}", "plain"),
            ("  \u{2605} task \t", "\u{2605} task"),
            (" \u{25d0}task \n", "\u{25d0}task"),
        ] {
            assert_eq!(stripped_terminal_title(title).as_deref(), Some(expected));
        }
        for title in ["", " \t\n", "\u{00a0}\u{2003}"] {
            assert_eq!(stripped_terminal_title(title), None);
        }
    }
}
