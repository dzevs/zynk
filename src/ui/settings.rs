use ratatui::{
    layout::{Constraint, Layout, Rect},
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{List, ListItem, ListState, Paragraph, Tabs},
    Frame,
};

use super::widgets::{
    action_button_row_rects, centered_popup_rect, modal_stack_areas, panel_contrast_fg,
    render_action_button, render_modal_choice_list, render_panel_shell, ActionButtonSpec,
};
use crate::{
    app::{
        state::{ExperimentSetting, HeaderSettingRow, Palette},
        AppState,
    },
    config::ToastDelivery,
};

pub(crate) const SETTINGS_POPUP_WIDTH: u16 = 76;
pub(crate) const SETTINGS_POPUP_BASE_HEIGHT: u16 = 22;

pub(crate) fn settings_popup_height(app: &AppState) -> u16 {
    if app.settings.section != crate::app::state::SettingsSection::Integrations {
        return SETTINGS_POPUP_BASE_HEIGHT;
    }
    let list_rows = app.integration_recommendations.len().max(1) as u16;
    let footer_rows = integrations_footer_height(app, SETTINGS_POPUP_WIDTH - 2);
    // borders 2 + header 3 + stack gaps 2 + modal footer 2
    // + section title 1 + description 2 + spacers 2
    (14 + list_rows + footer_rows).max(SETTINGS_POPUP_BASE_HEIGHT)
}

pub(super) fn render_settings_overlay(app: &AppState, frame: &mut Frame, area: Rect) {
    use crate::app::state::SettingsSection;

    let p = &app.palette;
    let Some(popup) = centered_popup_rect(area, SETTINGS_POPUP_WIDTH, settings_popup_height(app))
    else {
        return;
    };

    super::dim_background(frame, area);

    let Some(inner) = render_panel_shell(frame, popup, p.accent, p.panel_bg) else {
        return;
    };
    if inner.height < 4 || inner.width < 10 {
        return;
    }

    let stack = modal_stack_areas(inner, 3, 2, 0, 1);
    let header_rows = Layout::vertical([
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Length(1),
    ])
    .areas::<3>(stack.header);

    frame.render_widget(
        Paragraph::new(Line::from(vec![Span::styled(
            " settings",
            Style::default().fg(p.text).add_modifier(Modifier::BOLD),
        )])),
        header_rows[0],
    );

    let tab_labels = SettingsSection::ALL.iter().map(|section| {
        if app.settings_section_has_badge(*section) {
            Line::from(vec![
                Span::styled(
                    "● ",
                    Style::default().fg(p.accent).add_modifier(Modifier::BOLD),
                ),
                Span::raw(section.label()),
            ])
        } else {
            Line::from(section.label())
        }
    });
    let tabs = Tabs::new(tab_labels)
        .select(
            SettingsSection::ALL
                .iter()
                .position(|section| *section == app.settings.section)
                .unwrap_or(0),
        )
        .style(Style::default().fg(p.overlay1))
        .highlight_style(
            Style::default()
                .fg(panel_contrast_fg(p))
                .bg(p.accent)
                .add_modifier(Modifier::BOLD),
        )
        .divider(" ")
        .padding(" ", " ");
    frame.render_widget(tabs, header_rows[1]);

    let sep = "─".repeat(inner.width as usize);
    frame.render_widget(
        Paragraph::new(Span::styled(&sep, Style::default().fg(p.surface0))),
        header_rows[2],
    );

    let content_area = stack.content;

    match app.settings.section {
        SettingsSection::Theme => {
            render_settings_theme(app, frame, content_area);
        }
        SettingsSection::Sound => {
            render_settings_toggle(
                frame,
                content_area,
                p,
                "sound alerts",
                "play sounds when agents change state in background",
                app.sound_enabled(),
                app.settings.list.selected,
            );
        }
        SettingsSection::Toast => {
            render_modal_choice_list(
                frame,
                content_area,
                "notification popups",
                "choose where background popup notifications should appear",
                &[
                    ("off", ToastDelivery::Off),
                    ("inside zynk", ToastDelivery::Zynk),
                    ("via terminal", ToastDelivery::Terminal),
                    ("via system", ToastDelivery::System),
                ],
                app.toast_delivery(),
                app.settings.list.selected,
                p,
                2,
            );
        }
        SettingsSection::PaneLabels => {
            render_settings_toggle(
                frame,
                content_area,
                p,
                "agent border labels",
                "show detected agent names in split pane borders",
                app.agent_border_labels_enabled(),
                app.settings.list.selected,
            );
        }
        SettingsSection::Header => {
            render_settings_header(app, frame, content_area);
        }
        SettingsSection::Experiments => {
            render_settings_experiments(app, frame, content_area);
        }
        SettingsSection::Integrations => {
            render_settings_integrations(app, frame, content_area);
        }
    }

    if let Some(footer_area) = stack.footer {
        let footer_rows = Layout::vertical([Constraint::Length(1), Constraint::Length(1)])
            .areas::<2>(footer_area);
        let primary_label = settings_primary_button_label(app.settings.section);
        let show_primary = settings_show_primary_action(app);
        let (apply_rect, close_rect) =
            settings_button_rects(inner, app.settings.section, show_primary);
        if let Some(apply_rect) = apply_rect {
            render_action_button(
                frame,
                apply_rect,
                Some("↵"),
                primary_label,
                Style::default()
                    .fg(panel_contrast_fg(p))
                    .bg(p.accent)
                    .add_modifier(Modifier::BOLD),
            );
        }
        render_action_button(
            frame,
            close_rect,
            Some("esc"),
            "close",
            Style::default()
                .fg(p.text)
                .bg(p.surface0)
                .add_modifier(Modifier::BOLD),
        );

        frame.render_widget(
            Paragraph::new(Line::from(vec![
                Span::styled(" ↑↓", Style::default().fg(p.overlay0)),
                Span::styled(" select  ", Style::default().fg(p.overlay1)),
                Span::styled("tab", Style::default().fg(p.overlay0)),
                Span::styled(" section", Style::default().fg(p.overlay1)),
            ])),
            footer_rows[0],
        );
    }
}

pub(crate) fn settings_primary_button_label(
    section: crate::app::state::SettingsSection,
) -> &'static str {
    match section {
        crate::app::state::SettingsSection::Integrations => "install",
        _ => "apply",
    }
}

pub(crate) fn settings_show_primary_action(app: &AppState) -> bool {
    match app.settings.section {
        crate::app::state::SettingsSection::Integrations => app
            .integration_recommendations
            .iter()
            .any(crate::integration::IntegrationRecommendation::needs_install),
        _ => true,
    }
}

pub(crate) fn settings_button_rects(
    inner: Rect,
    section: crate::app::state::SettingsSection,
    show_primary: bool,
) -> (Option<Rect>, Rect) {
    if !show_primary {
        let rects = action_button_row_rects(
            inner,
            &[ActionButtonSpec {
                hint: Some("esc"),
                label: "close",
            }],
            2,
            inner.height.saturating_sub(1),
        );
        return (None, rects[0]);
    }

    let rects = action_button_row_rects(
        inner,
        &[
            ActionButtonSpec {
                hint: Some("↵"),
                label: settings_primary_button_label(section),
            },
            ActionButtonSpec {
                hint: Some("esc"),
                label: "close",
            },
        ],
        2,
        inner.height.saturating_sub(1),
    );
    (Some(rects[0]), rects[1])
}

fn integrations_footer_paragraph(app: &AppState) -> Paragraph<'static> {
    let p = &app.palette;
    let mut footer_lines = Vec::new();
    if !app.integration_install_messages.is_empty() {
        for message in &app.integration_install_messages {
            footer_lines.push(Line::from(Span::styled(
                format!(" {message}"),
                Style::default().fg(p.overlay1),
            )));
        }
    } else {
        let found_any = app.integration_recommendations.iter().any(|item| {
            item.available || item.state != crate::integration::IntegrationStatusKind::NotInstalled
        });
        let hint = if app
            .integration_recommendations
            .iter()
            .any(crate::integration::IntegrationRecommendation::needs_install)
        {
            " press install to add available or outdated integrations"
        } else if found_any {
            " all detected integrations are installed"
        } else {
            " no supported agent CLIs found on PATH"
        };
        footer_lines.push(Line::from(Span::styled(
            hint.to_string(),
            Style::default().fg(p.overlay1),
        )));
    }
    Paragraph::new(footer_lines).wrap(ratatui::widgets::Wrap { trim: false })
}

fn integrations_footer_height(app: &AppState, width: u16) -> u16 {
    (integrations_footer_paragraph(app).line_count(width) as u16).min(6)
}

fn render_settings_integrations(app: &AppState, frame: &mut Frame, area: Rect) {
    let p = &app.palette;

    let footer = integrations_footer_paragraph(app);
    let footer_height = integrations_footer_height(app, area.width);

    let rows = Layout::vertical([
        Constraint::Length(1),
        Constraint::Length(2),
        Constraint::Length(1),
        Constraint::Min(0),
        Constraint::Length(1),
        Constraint::Length(footer_height),
    ])
    .areas::<6>(area);

    frame.render_widget(
        Paragraph::new("agent integrations")
            .style(Style::default().fg(p.text).add_modifier(Modifier::BOLD)),
        rows[0],
    );
    frame.render_widget(
        Paragraph::new(
            "let agents report state directly instead of relying only on process detection",
        )
        .style(Style::default().fg(p.overlay1))
        .wrap(ratatui::widgets::Wrap { trim: false }),
        rows[1],
    );

    let mut lines = Vec::new();
    for item in &app.integration_recommendations {
        let marker = match item.state {
            crate::integration::IntegrationStatusKind::Current => "✓",
            crate::integration::IntegrationStatusKind::Outdated => "↻",
            crate::integration::IntegrationStatusKind::NotInstalled if item.available => "+",
            crate::integration::IntegrationStatusKind::NotInstalled => "–",
        };
        let marker_style = match item.state {
            crate::integration::IntegrationStatusKind::Current => Style::default().fg(p.green),
            crate::integration::IntegrationStatusKind::Outdated => Style::default().fg(p.yellow),
            crate::integration::IntegrationStatusKind::NotInstalled if item.available => {
                Style::default().fg(p.accent)
            }
            crate::integration::IntegrationStatusKind::NotInstalled => {
                Style::default().fg(p.overlay0)
            }
        };
        lines.push(Line::from(vec![
            Span::styled(format!(" {marker} "), marker_style),
            Span::styled(
                format!("{:<9}", item.label),
                Style::default().fg(p.subtext0),
            ),
            Span::styled(item.status_label(), Style::default().fg(p.overlay1)),
        ]));
    }

    if lines.is_empty() {
        lines.push(Line::from(Span::styled(
            " no integration targets available",
            Style::default().fg(p.overlay1),
        )));
    }

    frame.render_widget(Paragraph::new(lines), rows[3]);
    frame.render_widget(footer, rows[5]);
}

fn render_settings_theme(app: &AppState, frame: &mut Frame, area: Rect) {
    use crate::app::state::THEME_NAMES;

    let p = &app.palette;
    let items: Vec<ListItem> = THEME_NAMES
        .iter()
        .map(|name| {
            let is_current = name.to_lowercase().replace([' ', '_'], "-")
                == app.theme_name.to_lowercase().replace([' ', '_'], "-");
            let marker = if is_current { " ✓" } else { "" };
            ListItem::new(Line::from(vec![
                Span::styled(*name, Style::default().fg(p.subtext0)),
                Span::styled(marker, Style::default().fg(p.green)),
            ]))
        })
        .collect();

    let list = List::new(items)
        .highlight_style(
            Style::default()
                .bg(p.surface0)
                .fg(p.text)
                .add_modifier(Modifier::BOLD),
        )
        .highlight_symbol(" ▸ ")
        .style(Style::default().fg(p.subtext0));

    let mut state = ListState::default().with_selected(Some(app.settings.list.selected));
    frame.render_stateful_widget(list, area, &mut state);
}

fn render_settings_toggle(
    frame: &mut Frame,
    area: Rect,
    p: &Palette,
    title: &str,
    description: &str,
    current_value: bool,
    selected_idx: usize,
) {
    render_modal_choice_list(
        frame,
        area,
        title,
        description,
        &[("on", true), ("off", false)],
        current_value,
        selected_idx,
        p,
        1,
    );
}

/// Feature #116 (Q3): true when `ZYNK_HEADER_VERBOSE` is truthy (`1`/`true`,
/// case-insensitive, surrounding whitespace ignored) — mirrors `HeaderConfig::resolve`.
/// When set, the verbose row shows an "(overridden by env)" indicator.
fn header_verbose_env_override() -> bool {
    std::env::var(crate::config::HEADER_VERBOSE_ENV_VAR)
        .ok()
        .map(|raw| {
            let v = raw.trim();
            v.eq_ignore_ascii_case("1") || v.eq_ignore_ascii_case("true")
        })
        .unwrap_or(false)
}

/// Feature #116: the `[header]` Settings section — a `verbose` toggle row and a
/// `max_width < N >` stepper row. The header is applied per-send client-side, so a
/// dim hint explains there is no server reload, and an "(overridden by env)" tag is
/// appended to the verbose row when `ZYNK_HEADER_VERBOSE` is truthy (Q3).
fn render_settings_header(app: &AppState, frame: &mut Frame, area: Rect) {
    let p = &app.palette;
    let [desc_area, _, list_area, _, hint_area] = Layout::vertical([
        Constraint::Length(2),
        Constraint::Length(1),
        Constraint::Length(HeaderSettingRow::COUNT as u16),
        Constraint::Length(1),
        Constraint::Min(1),
    ])
    .areas::<5>(area);

    super::widgets::render_modal_description(
        frame,
        desc_area,
        "agent-visible message header shown on outgoing zynk messages",
        Style::default().fg(p.overlay1),
    );

    let env_override = header_verbose_env_override();
    let selected = app.settings.list.selected;

    let row_style = |idx: usize| {
        if selected == idx {
            Style::default()
                .bg(p.surface0)
                .fg(p.text)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(p.subtext0)
        }
    };

    // Row 0: verbose toggle — " verbose  [on/off]" (+ env-override tag when set).
    let verbose_value = if app.header_verbose() { "on" } else { "off" };
    let mut verbose_spans = vec![Span::styled(
        format!(" verbose  [{verbose_value}]"),
        row_style(0),
    )];
    if env_override {
        verbose_spans.push(Span::styled(
            "  (overridden by env)",
            Style::default().fg(p.overlay0).add_modifier(Modifier::DIM),
        ));
    }
    let verbose_row = Rect::new(list_area.x, list_area.y, list_area.width, 1);
    frame.render_widget(Paragraph::new(Line::from(verbose_spans)), verbose_row);

    // Row 1: max_width stepper — " max_width  < N >".
    let max_width_row = Rect::new(list_area.x, list_area.y + 1, list_area.width, 1);
    frame.render_widget(
        Paragraph::new(format!(" max_width  < {} >", app.header_max_width())).style(row_style(1)),
        max_width_row,
    );

    frame.render_widget(
        Paragraph::new(
            "Applies to outgoing zynk message headers (per-send). ZYNK_HEADER_VERBOSE env overrides verbose.",
        )
        .style(Style::default().fg(p.overlay1).add_modifier(Modifier::DIM))
        .wrap(ratatui::widgets::Wrap { trim: false }),
        hint_area,
    );
}

fn render_settings_experiments(app: &AppState, frame: &mut Frame, area: Rect) {
    let p = &app.palette;
    let [desc_area, _, list_area] = Layout::vertical([
        Constraint::Length(2),
        Constraint::Length(1),
        Constraint::Min(1),
    ])
    .areas::<3>(area);

    super::widgets::render_modal_description(
        frame,
        desc_area,
        "optional features that are off by default",
        Style::default().fg(p.overlay1),
    );

    for (idx, setting) in ExperimentSetting::ALL.iter().copied().enumerate() {
        let marker = if setting.enabled(app) { "[✓]" } else { "[ ]" };
        let style = if app.settings.list.selected == idx {
            Style::default()
                .bg(p.surface0)
                .fg(p.text)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(p.subtext0)
        };
        let row = Rect::new(list_area.x, list_area.y + idx as u16, list_area.width, 1);
        frame.render_widget(
            Paragraph::new(format!(" {} {marker}", setting.label())).style(style),
            row,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::{state::SettingsSection, Mode};
    use ratatui::{backend::TestBackend, Terminal};

    #[test]
    fn experiments_pane_history_uses_settings_checkmark_marker() {
        let mut app = AppState::test_new();
        app.pane_history_persistence = true;
        app.settings.section = SettingsSection::Experiments;
        app.settings.list.selected = 0;
        app.mode = Mode::Settings;

        let mut terminal =
            Terminal::new(TestBackend::new(80, 24)).expect("test terminal should initialize");
        terminal
            .draw(|frame| render_settings_overlay(&app, frame, Rect::new(0, 0, 80, 24)))
            .expect("settings overlay should render");

        let rendered = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|cell| cell.symbol())
            .collect::<String>();

        assert!(rendered.contains("pane screen history [✓]"));
        assert!(!rendered.contains("[x]"));
    }

    #[test]
    fn experiments_pane_history_keeps_empty_checkbox_marker_when_disabled() {
        let mut app = AppState::test_new();
        app.pane_history_persistence = false;
        app.settings.section = SettingsSection::Experiments;
        app.settings.list.selected = 0;
        app.mode = Mode::Settings;

        let mut terminal =
            Terminal::new(TestBackend::new(80, 24)).expect("test terminal should initialize");
        terminal
            .draw(|frame| render_settings_overlay(&app, frame, Rect::new(0, 0, 80, 24)))
            .expect("settings overlay should render");

        let rendered = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|cell| cell.symbol())
            .collect::<String>();

        assert!(rendered.contains("pane screen history [ ]"));
    }

    fn render_to_string(app: &AppState) -> String {
        let mut terminal =
            Terminal::new(TestBackend::new(80, 24)).expect("test terminal should initialize");
        terminal
            .draw(|frame| render_settings_overlay(app, frame, Rect::new(0, 0, 80, 24)))
            .expect("settings overlay should render");
        terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|cell| cell.symbol())
            .collect::<String>()
    }

    #[test]
    fn settings_tab_row_renders_rightmost_label_without_clipping() {
        // #116 regression: the rightmost tab (Experiments, last in `ALL`) must
        // render in full. Pre-fix the 76-col tab row overflowed the 74-col inner
        // width and the trailing "s" was clipped to "experiment".
        let mut app = AppState::test_new();
        app.settings.section = SettingsSection::Theme;
        app.mode = Mode::Settings;
        let rendered = render_to_string(&app);
        assert!(
            rendered.contains("experiments"),
            "rightmost settings tab was clipped: {rendered:?}"
        );
        // The shortened pane-labels tab label is present (content title stays
        // "agent border labels", asserted elsewhere).
        assert!(rendered.contains("labels"), "pane-labels tab missing");
    }

    // Feature #116: the Header section renders the verbose toggle and the max_width
    // stepper; the env-override indicator shows only when ZYNK_HEADER_VERBOSE is truthy.
    #[test]
    fn header_section_renders_rows_and_env_override_indicator() {
        let _guard = crate::config::test_config_env_lock().lock().unwrap();
        std::env::remove_var(crate::config::HEADER_VERBOSE_ENV_VAR);

        let mut app = AppState::test_new();
        app.header_verbose = false;
        app.header_max_width = 64;
        app.settings.section = SettingsSection::Header;
        app.settings.list.selected = 0;
        app.mode = Mode::Settings;

        let rendered = render_to_string(&app);
        assert!(
            rendered.contains("verbose  [off]"),
            "verbose row: {rendered}"
        );
        assert!(
            rendered.contains("max_width  < 64 >"),
            "stepper row: {rendered}"
        );
        // The hint can wrap across rows in the 80-col test buffer; assert its parts.
        assert!(
            rendered.contains("Applies to outgoing zynk message headers (per-send)."),
            "hint line: {rendered}"
        );
        assert!(
            rendered.contains("ZYNK_HEADER_VERBOSE"),
            "hint mentions env var: {rendered}"
        );
        assert!(
            !rendered.contains("overridden by env"),
            "no env tag when unset: {rendered}"
        );

        // With the env var truthy, the override indicator appears on the verbose row.
        std::env::set_var(crate::config::HEADER_VERBOSE_ENV_VAR, "1");
        let rendered = render_to_string(&app);
        assert!(
            rendered.contains("overridden by env"),
            "env tag shows when ZYNK_HEADER_VERBOSE=1: {rendered}"
        );
        std::env::remove_var(crate::config::HEADER_VERBOSE_ENV_VAR);
    }

    #[test]
    fn experiments_renders_switch_ascii_input_source_row() {
        let mut app = AppState::test_new();
        app.switch_ascii_input_source_in_prefix = true;
        app.settings.section = SettingsSection::Experiments;
        app.settings.list.selected = 1;
        app.mode = Mode::Settings;

        let mut terminal =
            Terminal::new(TestBackend::new(80, 24)).expect("test terminal should initialize");
        terminal
            .draw(|frame| render_settings_overlay(&app, frame, Rect::new(0, 0, 80, 24)))
            .expect("settings overlay should render");

        let rendered = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|cell| cell.symbol())
            .collect::<String>();

        assert!(rendered.contains("switch to ascii input source in prefix (macOS) [✓]"));
    }
}
