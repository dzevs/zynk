//! Display-width helpers for UI CHROME labels (tab titles, sidebar rows, dialog
//! rows, pane border titles, toasts).
//!
//! Boundary: this module is the single chrome-width oracle and measures with
//! `unicode_width`. Terminal CELL geometry — `visible_text_cells`,
//! `logical_cell_for_visible_cell`, `last_character_col`, `text_cells`,
//! `char_cell_width` — must keep using `crate::ghostty::unicode_codepoint_width`
//! / `unicode_grapheme_width`, which is the same width table the emulator uses
//! when it lays text into the grid. Mixing the two would let chrome and grid
//! disagree about the same string.

use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

pub(crate) fn display_width(text: &str) -> usize {
    UnicodeWidthStr::width(text)
}

pub(crate) fn display_width_u16(text: &str) -> u16 {
    display_width(text).min(u16::MAX as usize) as u16
}

pub(crate) fn truncate_end(text: &str, max_width: usize) -> String {
    if display_width(text) <= max_width {
        return text.to_string();
    }
    if max_width == 0 {
        return String::new();
    }
    if max_width == 1 {
        return "…".to_string();
    }

    let prefix = take_prefix_width(text, max_width.saturating_sub(1));
    format!("{prefix}…")
}

pub(crate) fn middle_elide(text: &str, max_width: usize) -> String {
    if display_width(text) <= max_width {
        return text.to_string();
    }
    if max_width <= 1 {
        return "…".to_string();
    }

    let content_width = max_width.saturating_sub(1);
    let left_width = content_width / 2;
    let right_width = content_width.saturating_sub(left_width);
    let prefix = take_prefix_width(text, left_width);
    let suffix = take_suffix_width(text, right_width);
    format!("{prefix}…{suffix}")
}

fn take_prefix_width(text: &str, max_width: usize) -> String {
    let mut output = String::new();
    let mut width = 0usize;
    for ch in text.chars() {
        let ch_width = UnicodeWidthChar::width(ch).unwrap_or(0);
        if width + ch_width > max_width {
            break;
        }
        output.push(ch);
        width += ch_width;
    }
    output
}

fn take_suffix_width(text: &str, max_width: usize) -> String {
    let mut output = Vec::new();
    let mut width = 0usize;
    for ch in text.chars().rev() {
        let ch_width = UnicodeWidthChar::width(ch).unwrap_or(0);
        if width + ch_width > max_width {
            break;
        }
        output.push(ch);
        width += ch_width;
    }
    output.into_iter().rev().collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn truncate_end_uses_display_width() {
        let text = truncate_end("提交 zynk 的反馈", 15);

        assert_eq!(text, "提交 zynk 的反…");
        assert!(display_width(&text) <= 15);
    }

    #[test]
    fn middle_elide_uses_display_width() {
        let text = middle_elide("重构用户认证模块并迁移到统一登录服务", 12);

        assert!(text.contains('…'));
        assert!(display_width(&text) <= 12);
    }

    /// Fork-only guard for the chrome/cell boundary documented above: the chrome
    /// oracle must agree with the terminal's own cell-width table, so a CJK or
    /// emoji chrome label truncates to exactly the number of grid columns the
    /// emulator would have used. The cell-geometry paths keep calling
    /// `crate::ghostty::unicode_codepoint_width` directly.
    #[test]
    fn chrome_display_width_agrees_with_the_terminal_cell_oracle() {
        for ch in ['界', '模', '交', '⠋', 'A', 'x'] {
            assert_eq!(
                display_width(&ch.to_string()),
                usize::from(crate::ghostty::unicode_codepoint_width(ch as u32)),
                "chrome width disagrees with the terminal cell oracle for {ch:?}"
            );
        }
        assert_eq!(display_width("模块组织"), 8);
        assert_eq!(take_prefix_width("模块组织", 5), "模块");
        assert_eq!(take_suffix_width("模块组织", 5), "组织");
        // A wide char is never split across the budget boundary.
        assert_eq!(display_width(&truncate_end("模块组织", 5)), 5);
    }
}
