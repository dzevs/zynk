// Modified by the zynk project: this file differs from the upstream version it was derived from.
// See NOTICE ("Modified files (Apache-2.0 provenance)") for the provenance and the license terms.
use ratatui::{
    layout::{Constraint, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, Paragraph},
    Frame,
};

use super::text::display_width_u16;
use super::widgets::panel_contrast_fg;
use crate::{
    app::state::{CopyFeedback, Palette, ToastKind, ToastNotification},
    config::{StatusIndicatorStyle, ToastClipboardPosition, ToastZynkPosition},
    detect::AgentState,
};

const WORKING_SPINNER_FRAMES: &[&str] = &["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];

pub(super) fn spinner_frame(tick: u32) -> &'static str {
    WORKING_SPINNER_FRAMES[(tick as usize / 8) % WORKING_SPINNER_FRAMES.len()]
}

pub(super) fn is_spinner_frame(symbol: &str) -> bool {
    WORKING_SPINNER_FRAMES.contains(&symbol)
}

pub(crate) fn copy_feedback_rect(
    area: Rect,
    feedback: &CopyFeedback,
    offset_rows: u16,
    position: ToastClipboardPosition,
) -> Rect {
    if area.width == 0 || area.height == 0 {
        return Rect::default();
    }

    let content_width = feedback.message.len() as u16 + 4;
    let width = content_width.min(area.width);
    let height = 3u16.min(area.height);
    let x = match position {
        ToastClipboardPosition::TopLeft | ToastClipboardPosition::BottomLeft => area.x,
        ToastClipboardPosition::TopCenter | ToastClipboardPosition::BottomCenter => {
            area.x + area.width.saturating_sub(width) / 2
        }
        ToastClipboardPosition::TopRight | ToastClipboardPosition::BottomRight => {
            area.x + area.width.saturating_sub(width)
        }
    };
    let y = match position {
        ToastClipboardPosition::TopLeft
        | ToastClipboardPosition::TopCenter
        | ToastClipboardPosition::TopRight => area.y + offset_rows.min(area.height),
        ToastClipboardPosition::BottomLeft
        | ToastClipboardPosition::BottomCenter
        | ToastClipboardPosition::BottomRight => {
            area.y + area.height.saturating_sub(height + offset_rows)
        }
    };
    Rect::new(x, y, width, height)
}

pub(crate) fn toast_notification_rect(
    area: Rect,
    toast: &ToastNotification,
    offset_for_warning: bool,
    position: ToastZynkPosition,
) -> Rect {
    let content_width = display_width_u16(&toast.title)
        .max(display_width_u16(&toast.context))
        .saturating_add(4);
    let width = content_width.saturating_add(2).min(area.width);
    let content_height = if toast.context.is_empty() { 1 } else { 2 };
    let height = (content_height + 2).min(area.height);
    let x = match position {
        ToastZynkPosition::TopLeft | ToastZynkPosition::BottomLeft => area.x,
        ToastZynkPosition::TopRight | ToastZynkPosition::BottomRight => {
            area.x + area.width.saturating_sub(width)
        }
    };
    let warning_offset = u16::from(offset_for_warning);
    let y = match position {
        ToastZynkPosition::TopLeft | ToastZynkPosition::TopRight => {
            area.y + warning_offset.min(area.height)
        }
        ToastZynkPosition::BottomLeft | ToastZynkPosition::BottomRight => {
            area.y + area.height.saturating_sub(height + warning_offset)
        }
    };
    Rect::new(x, y, width, height)
}

pub(super) fn render_toast_notification(
    frame: &mut Frame,
    area: Rect,
    toast: &ToastNotification,
    offset_for_warning: bool,
    position: ToastZynkPosition,
    p: &Palette,
) {
    let dot_color = match toast.kind {
        ToastKind::NeedsAttention => p.red,
        ToastKind::Finished => p.blue,
        ToastKind::UpdateInstalled => p.accent,
    };
    let toast_area = toast_notification_rect(area, toast, offset_for_warning, position);

    frame.render_widget(Clear, toast_area);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(p.overlay0))
        .style(Style::default().bg(p.panel_bg));
    let inner = block.inner(toast_area);
    frame.render_widget(block, toast_area);

    if inner.height < 1 {
        return;
    }

    let [title_row, context_row] =
        Layout::vertical([Constraint::Length(1), Constraint::Length(1)]).areas(inner);

    let title = Line::from(vec![
        Span::styled("●", Style::default().fg(dot_color)),
        Span::raw(" "),
        Span::styled(
            &toast.title,
            Style::default().fg(p.text).add_modifier(Modifier::BOLD),
        ),
    ]);
    let context = Line::from(vec![
        Span::styled("  ", Style::default().fg(p.overlay0)),
        Span::styled(&toast.context, Style::default().fg(p.overlay0)),
    ]);

    frame.render_widget(Paragraph::new(title), title_row);
    if !toast.context.is_empty() && inner.height >= 2 {
        frame.render_widget(Paragraph::new(context), context_row);
    }
}

pub(super) fn render_copy_feedback(
    frame: &mut Frame,
    area: Rect,
    feedback: &CopyFeedback,
    offset_rows: u16,
    position: ToastClipboardPosition,
    p: &Palette,
) {
    let feedback_area = copy_feedback_rect(area, feedback, offset_rows, position);
    if feedback_area.is_empty() {
        return;
    }

    frame.render_widget(Clear, feedback_area);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(p.green))
        .style(Style::default().bg(p.panel_bg));
    let inner = block.inner(feedback_area);
    frame.render_widget(block, feedback_area);

    if inner.height == 0 {
        return;
    }

    let text = Line::from(vec![
        Span::styled("●", Style::default().fg(p.green).bg(p.panel_bg)),
        Span::raw(" "),
        Span::styled(
            &feedback.message,
            Style::default()
                .fg(p.text)
                .bg(p.panel_bg)
                .add_modifier(Modifier::BOLD),
        ),
    ]);
    frame.render_widget(Paragraph::new(text), inner);
}

pub(super) fn render_config_diagnostic(frame: &mut Frame, area: Rect, message: &str, p: &Palette) {
    let style = Style::default()
        .fg(panel_contrast_fg(p))
        .bg(p.yellow)
        .add_modifier(Modifier::BOLD);

    for (row, line) in message
        .lines()
        .filter(|line| !line.trim().is_empty())
        .take(area.height as usize)
        .enumerate()
    {
        let text = format!(" {line} ");
        let width = (text.len() as u16).min(area.width);
        let notif_area = Rect::new(
            area.x + area.width.saturating_sub(width),
            area.y + row as u16,
            width,
            1,
        );

        frame.render_widget(Clear, notif_area);
        frame.render_widget(Paragraph::new(Span::styled(text, style)), notif_area);
    }
}

pub(super) fn state_icon_symbol(
    state: AgentState,
    seen: bool,
    indicator_style: StatusIndicatorStyle,
) -> &'static str {
    match (indicator_style, state, seen) {
        (StatusIndicatorStyle::Dots, AgentState::Blocked, _) => "●",
        (StatusIndicatorStyle::Dots, AgentState::Working, _) => "●",
        (StatusIndicatorStyle::Dots, AgentState::Idle, false) => "●",
        (StatusIndicatorStyle::Dots, AgentState::Idle, true) => "○",
        (StatusIndicatorStyle::Dots, AgentState::Unknown, _) => "·",
        (StatusIndicatorStyle::Symbols, AgentState::Blocked, _) => "×",
        (StatusIndicatorStyle::Symbols, AgentState::Working, _) => "◐",
        (StatusIndicatorStyle::Symbols, AgentState::Idle, false) => "✓",
        (StatusIndicatorStyle::Symbols, AgentState::Idle, true) => "○",
        (StatusIndicatorStyle::Symbols, AgentState::Unknown, _) => "·",
    }
}

pub(super) fn state_icon(
    state: AgentState,
    seen: bool,
    indicator_style: StatusIndicatorStyle,
    p: &Palette,
) -> (&'static str, Style) {
    (
        state_icon_symbol(state, seen, indicator_style),
        Style::default().fg(state_label_color(state, seen, p)),
    )
}

pub(super) fn agent_icon(
    state: AgentState,
    seen: bool,
    indicator_style: StatusIndicatorStyle,
    working_animation: bool,
    tick: u32,
    p: &Palette,
) -> (&'static str, Style) {
    if state == AgentState::Working && working_animation {
        return (spinner_frame(tick), Style::default().fg(p.yellow));
    }
    if indicator_style == StatusIndicatorStyle::Symbols {
        return state_icon(state, seen, indicator_style, p);
    }
    match (state, seen) {
        (AgentState::Blocked, _) => ("◉", Style::default().fg(p.red)),
        (AgentState::Working, _) => ("●", Style::default().fg(p.yellow)),
        (AgentState::Idle, false) => ("●", Style::default().fg(p.teal)),
        (AgentState::Idle, true) => ("✓", Style::default().fg(p.green)),
        (AgentState::Unknown, _) => ("○", Style::default().fg(p.overlay0)),
    }
}

pub(super) fn state_label(state: AgentState, seen: bool) -> &'static str {
    match (state, seen) {
        (AgentState::Blocked, _) => "blocked",
        (AgentState::Working, _) => "working",
        (AgentState::Idle, false) => "done",
        (AgentState::Idle, true) => "idle",
        (AgentState::Unknown, _) => "idle",
    }
}

pub(super) fn state_label_color(state: AgentState, seen: bool, p: &Palette) -> Color {
    match (state, seen) {
        (AgentState::Blocked, _) => p.red,
        (AgentState::Working, _) => p.yellow,
        (AgentState::Idle, false) => p.teal,
        (AgentState::Idle, true) => p.green,
        (AgentState::Unknown, _) => p.overlay0,
    }
}

pub(crate) const WORKING_LABEL: &str = "working";
pub(crate) const WORKING_LABEL_LEN: usize = 7;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct WorkingShimmerPalette {
    pub(crate) base: crate::terminal_theme::RgbColor,
    pub(crate) target: crate::terminal_theme::RgbColor,
}

pub(crate) fn working_label_shimmer_palette(
    base: Color,
    target: Color,
    host: &crate::terminal_theme::TerminalTheme,
) -> Option<WorkingShimmerPalette> {
    Some(WorkingShimmerPalette {
        base: resolve_shimmer_color(base, host)?,
        target: resolve_shimmer_color(target, host)?,
    })
}

pub(crate) fn working_label_shimmer_color(
    palette: WorkingShimmerPalette,
    tick: u32,
    character_index: usize,
) -> Color {
    let weight = working_label_shimmer_weight(tick, character_index);
    let blended = blend_quarters(palette.base, palette.target, weight);
    Color::Rgb(blended.r, blended.g, blended.b)
}

pub(crate) fn working_label_shimmer_weight(tick: u32, character_index: usize) -> u16 {
    let step = ((tick / crate::app::WORKING_ANIMATION_TICK_STEP) % 10) as usize;
    match step.checked_sub(character_index) {
        Some(0) => 3,
        Some(1) => 2,
        Some(2) => 1,
        _ => 0,
    }
}

pub(super) fn working_label_spans(
    label: String,
    style: Style,
    animate_default_working: bool,
    app: &crate::app::state::AppState,
) -> Vec<Span<'static>> {
    if !animate_default_working || !app.working_animation || label != WORKING_LABEL {
        return vec![Span::styled(label, style)];
    }
    let Some(palette) = working_label_shimmer_palette(
        style.fg.unwrap_or(app.palette.yellow),
        app.palette.text,
        &app.host_terminal_theme,
    ) else {
        return vec![Span::styled(label, style)];
    };
    label
        .chars()
        .enumerate()
        .map(|(index, character)| {
            Span::styled(
                character.to_string(),
                style.fg(working_label_shimmer_color(
                    palette,
                    app.spinner_tick,
                    index,
                )),
            )
        })
        .collect()
}

#[cfg(test)]
fn working_label_shimmer_colors(
    base: Color,
    target: Color,
    tick: u32,
    host: &crate::terminal_theme::TerminalTheme,
) -> Option<[Color; WORKING_LABEL_LEN]> {
    let palette = working_label_shimmer_palette(base, target, host)?;
    Some(std::array::from_fn(|index| {
        working_label_shimmer_color(palette, tick, index)
    }))
}

fn blend_quarters(
    base: crate::terminal_theme::RgbColor,
    target: crate::terminal_theme::RgbColor,
    weight: u16,
) -> crate::terminal_theme::RgbColor {
    let blend = |base: u8, target: u8| {
        let value = u16::from(base) * (4 - weight) + u16::from(target) * weight + 2;
        (value / 4) as u8
    };
    crate::terminal_theme::RgbColor {
        r: blend(base.r, target.r),
        g: blend(base.g, target.g),
        b: blend(base.b, target.b),
    }
}

fn resolve_shimmer_color(
    color: Color,
    host: &crate::terminal_theme::TerminalTheme,
) -> Option<crate::terminal_theme::RgbColor> {
    let palette_index = match color {
        Color::Black => Some(0),
        Color::Red => Some(1),
        Color::Green => Some(2),
        Color::Yellow => Some(3),
        Color::Blue => Some(4),
        Color::Magenta => Some(5),
        Color::Cyan => Some(6),
        Color::Gray => Some(7),
        Color::DarkGray => Some(8),
        Color::LightRed => Some(9),
        Color::LightGreen => Some(10),
        Color::LightYellow => Some(11),
        Color::LightBlue => Some(12),
        Color::LightMagenta => Some(13),
        Color::LightCyan => Some(14),
        Color::White => Some(15),
        Color::Indexed(index) => Some(usize::from(index)),
        Color::Reset => return host.foreground,
        Color::Rgb(r, g, b) => return Some(crate::terminal_theme::RgbColor { r, g, b }),
    };
    host.palette[palette_index?]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{ToastClipboardPosition, ToastZynkPosition};

    fn toast() -> ToastNotification {
        ToastNotification {
            kind: ToastKind::Finished,
            title: "done".to_string(),
            context: "workspace".to_string(),
            position: None,
            target: None,
        }
    }

    fn feedback() -> CopyFeedback {
        CopyFeedback {
            message: "copied to clipboard".to_string(),
        }
    }

    #[test]
    fn working_animation_composes_with_navigator_and_mobile_grammar() {
        let p = crate::app::state::Palette::tokyo_night();
        let (gw, sw) = agent_icon(
            AgentState::Working,
            false,
            StatusIndicatorStyle::Dots,
            true,
            8,
            &p,
        );
        assert_eq!(gw, "⠙");
        assert_eq!(sw.fg, Some(p.yellow));
        assert_eq!(
            agent_icon(
                AgentState::Working,
                false,
                StatusIndicatorStyle::Symbols,
                true,
                16,
                &p,
            )
            .0,
            "⠹"
        );
        assert_eq!(
            agent_icon(
                AgentState::Working,
                false,
                StatusIndicatorStyle::Dots,
                false,
                8,
                &p,
            )
            .0,
            "●"
        );
        assert_eq!(
            agent_icon(
                AgentState::Working,
                false,
                StatusIndicatorStyle::Symbols,
                false,
                8,
                &p,
            )
            .0,
            "◐"
        );
        // idle keeps ● (done/unseen) / ✓ (idle/seen), NOT ○.
        assert_eq!(
            agent_icon(
                AgentState::Idle,
                false,
                StatusIndicatorStyle::Dots,
                true,
                8,
                &p,
            )
            .0,
            "●"
        );
        assert_eq!(
            agent_icon(
                AgentState::Idle,
                true,
                StatusIndicatorStyle::Dots,
                false,
                8,
                &p,
            )
            .0,
            "✓"
        );
        // unknown keeps ○, NOT ◌.
        assert_eq!(
            agent_icon(
                AgentState::Unknown,
                false,
                StatusIndicatorStyle::Dots,
                true,
                8,
                &p,
            )
            .0,
            "○"
        );
        // blocked already ◉ red.
        let (gb, sb) = agent_icon(
            AgentState::Blocked,
            false,
            StatusIndicatorStyle::Dots,
            true,
            8,
            &p,
        );
        assert_eq!(gb, "◉");
        assert_eq!(sb.fg, Some(p.red));
    }

    #[test]
    fn working_label_shimmer_uses_the_exact_quarter_blend_sequence() {
        let base = Color::Rgb(20, 40, 60);
        let target = Color::Rgb(100, 120, 140);
        let expected = [
            [Color::Rgb(80, 100, 120), base, base, base, base, base, base],
            [
                Color::Rgb(60, 80, 100),
                Color::Rgb(80, 100, 120),
                base,
                base,
                base,
                base,
                base,
            ],
            [
                Color::Rgb(40, 60, 80),
                Color::Rgb(60, 80, 100),
                Color::Rgb(80, 100, 120),
                base,
                base,
                base,
                base,
            ],
            [
                base,
                Color::Rgb(40, 60, 80),
                Color::Rgb(60, 80, 100),
                Color::Rgb(80, 100, 120),
                base,
                base,
                base,
            ],
            [
                base,
                base,
                Color::Rgb(40, 60, 80),
                Color::Rgb(60, 80, 100),
                Color::Rgb(80, 100, 120),
                base,
                base,
            ],
            [
                base,
                base,
                base,
                Color::Rgb(40, 60, 80),
                Color::Rgb(60, 80, 100),
                Color::Rgb(80, 100, 120),
                base,
            ],
            [
                base,
                base,
                base,
                base,
                Color::Rgb(40, 60, 80),
                Color::Rgb(60, 80, 100),
                Color::Rgb(80, 100, 120),
            ],
            [
                base,
                base,
                base,
                base,
                base,
                Color::Rgb(40, 60, 80),
                Color::Rgb(60, 80, 100),
            ],
            [base, base, base, base, base, base, Color::Rgb(40, 60, 80)],
            [base; 7],
        ];

        for (step, expected) in expected.into_iter().enumerate() {
            assert_eq!(
                working_label_shimmer_colors(base, target, step as u32 * 8, &Default::default()),
                Some(expected),
                "step {step}"
            );
        }
        assert_eq!(
            working_label_shimmer_colors(base, target, 80, &Default::default()),
            working_label_shimmer_colors(base, target, 0, &Default::default())
        );
    }

    #[test]
    fn working_label_shimmer_resolves_host_colors_or_stays_static() {
        let host = crate::terminal_theme::TerminalTheme::default()
            .with_color(
                crate::terminal_theme::DefaultColorKind::Foreground,
                crate::terminal_theme::RgbColor {
                    r: 240,
                    g: 241,
                    b: 242,
                },
            )
            .with_palette_color(
                3,
                crate::terminal_theme::RgbColor {
                    r: 30,
                    g: 31,
                    b: 32,
                },
            )
            .with_palette_color(
                11,
                crate::terminal_theme::RgbColor {
                    r: 110,
                    g: 111,
                    b: 112,
                },
            );
        assert_eq!(
            resolve_shimmer_color(Color::Reset, &host),
            Some(crate::terminal_theme::RgbColor {
                r: 240,
                g: 241,
                b: 242
            })
        );
        assert_eq!(
            resolve_shimmer_color(Color::Indexed(3), &host),
            Some(crate::terminal_theme::RgbColor {
                r: 30,
                g: 31,
                b: 32
            })
        );
        assert_eq!(
            resolve_shimmer_color(Color::LightYellow, &host),
            Some(crate::terminal_theme::RgbColor {
                r: 110,
                g: 111,
                b: 112
            })
        );
        assert_eq!(
            working_label_shimmer_colors(Color::Indexed(4), Color::Reset, 0, &host),
            None
        );
    }

    #[test]
    fn working_label_shimmer_uses_the_rendered_label_color_as_its_base() {
        let mut app = crate::app::state::AppState::test_new();
        app.palette.yellow = Color::Rgb(200, 180, 20);
        app.palette.text = Color::Rgb(100, 120, 140);
        app.spinner_tick = 0;
        let rendered_base = Color::Rgb(12, 24, 36);
        let spans = working_label_spans(
            WORKING_LABEL.to_string(),
            Style::default().fg(rendered_base),
            true,
            &app,
        );
        let expected = working_label_shimmer_colors(
            rendered_base,
            app.palette.text,
            0,
            &app.host_terminal_theme,
        )
        .unwrap();
        assert_eq!(spans.len(), WORKING_LABEL_LEN);
        assert_eq!(
            spans
                .iter()
                .map(|span| span.style.fg.unwrap())
                .collect::<Vec<_>>(),
            expected
        );
    }

    #[test]
    fn working_label_shimmer_is_static_when_disabled_or_not_the_builtin_label() {
        let mut app = crate::app::state::AppState::test_new();
        app.palette.yellow = Color::Rgb(20, 40, 60);
        app.palette.text = Color::Rgb(100, 120, 140);
        app.spinner_tick = 16;
        let style = Style::default()
            .fg(app.palette.yellow)
            .bg(Color::Rgb(1, 2, 3))
            .add_modifier(Modifier::BOLD);

        app.working_animation = false;
        let off = working_label_spans(WORKING_LABEL.to_string(), style, true, &app);
        assert_eq!(off, vec![Span::styled(WORKING_LABEL.to_string(), style)]);

        app.working_animation = true;
        let custom = working_label_spans("building".to_string(), style, true, &app);
        assert_eq!(custom, vec![Span::styled("building".to_string(), style)]);
        let nonworking = working_label_spans(WORKING_LABEL.to_string(), style, false, &app);
        assert_eq!(
            nonworking,
            vec![Span::styled(WORKING_LABEL.to_string(), style)]
        );
    }

    #[test]
    fn working_spinner_uses_the_exact_v301_braille_sequence() {
        let expected = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];
        for (index, glyph) in expected.into_iter().enumerate() {
            assert_eq!(spinner_frame((index as u32) * 8), glyph);
        }
        assert_eq!(spinner_frame(80), "⠋");
    }

    #[test]
    fn state_icons_support_dot_and_distinct_symbol_styles() {
        let palette = Palette::catppuccin();
        for (indicator_style, expected_symbols) in [
            (StatusIndicatorStyle::Dots, ["●", "●", "●", "○", "·"]),
            (StatusIndicatorStyle::Symbols, ["×", "◐", "✓", "○", "·"]),
        ] {
            for ((state, seen, color), expected_symbol) in [
                (AgentState::Blocked, true, palette.red),
                (AgentState::Working, true, palette.yellow),
                (AgentState::Idle, false, palette.teal),
                (AgentState::Idle, true, palette.green),
                (AgentState::Unknown, true, palette.overlay0),
            ]
            .into_iter()
            .zip(expected_symbols)
            {
                let (actual_symbol, style) = state_icon(state, seen, indicator_style, &palette);
                assert_eq!(actual_symbol, expected_symbol);
                assert_eq!(display_width_u16(actual_symbol), 1);
                assert_eq!(style.fg, Some(color));
            }
        }
    }

    #[test]
    fn toast_rect_uses_configured_corner() {
        let area = Rect::new(10, 20, 100, 40);
        let toast = toast();

        let top_left = toast_notification_rect(area, &toast, false, ToastZynkPosition::TopLeft);
        assert_eq!(top_left.x, area.x);
        assert_eq!(top_left.y, area.y);

        let top_right = toast_notification_rect(area, &toast, false, ToastZynkPosition::TopRight);
        assert_eq!(top_right.x + top_right.width, area.x + area.width);
        assert_eq!(top_right.y, area.y);

        let bottom_left =
            toast_notification_rect(area, &toast, false, ToastZynkPosition::BottomLeft);
        assert_eq!(bottom_left.x, area.x);
        assert_eq!(bottom_left.y + bottom_left.height, area.y + area.height);

        let bottom_right =
            toast_notification_rect(area, &toast, false, ToastZynkPosition::BottomRight);
        assert_eq!(bottom_right.x + bottom_right.width, area.x + area.width);
        assert_eq!(bottom_right.y + bottom_right.height, area.y + area.height);
    }

    #[test]
    fn toast_rect_uses_display_width_for_cjk_labels() {
        let area = Rect::new(0, 0, 100, 20);
        let toast = ToastNotification {
            kind: ToastKind::NeedsAttention,
            title: "重构用户认证模块".to_string(),
            context: "提交 zynk 的反馈".to_string(),
            position: None,
            target: None,
        };

        let rect = toast_notification_rect(area, &toast, false, ToastZynkPosition::TopRight);

        let expected_content_width =
            display_width_u16(&toast.title).max(display_width_u16(&toast.context)) + 6;
        assert_eq!(rect.width, expected_content_width);
        assert_eq!(rect.x + rect.width, area.x + area.width);
    }

    #[test]
    fn copy_feedback_rect_uses_configured_position() {
        let area = Rect::new(10, 20, 100, 40);
        let feedback = feedback();

        let top_center = copy_feedback_rect(area, &feedback, 0, ToastClipboardPosition::TopCenter);
        assert_eq!(top_center.y, area.y);
        assert_eq!(
            top_center.x,
            area.x + area.width.saturating_sub(top_center.width) / 2
        );

        let bottom_center =
            copy_feedback_rect(area, &feedback, 0, ToastClipboardPosition::BottomCenter);
        assert_eq!(bottom_center.y + bottom_center.height, area.y + area.height);
        assert_eq!(
            bottom_center.x,
            area.x + area.width.saturating_sub(bottom_center.width) / 2
        );
    }
}
