// Modified by the zynk project: this file differs from the upstream version it was derived from.
// See NOTICE ("Modified files (Apache-2.0 provenance)") for the provenance and the license terms.
//! Server-rendered outer terminal window title.

use super::App;
use crate::config::{WindowTitlePart, WindowTitleTemplate, WindowTitleToken};

impl App {
    pub(crate) fn configure_window_title(&mut self, template: &str) {
        self.window_title_template =
            WindowTitleTemplate::parse(template)
                .ok()
                .flatten()
                .map(|template| {
                    let hostname = if template.uses(WindowTitleToken::Hostname) {
                        crate::platform::hostname().unwrap_or_default()
                    } else {
                        String::new()
                    };
                    (template, hostname)
                });
    }

    pub(crate) fn window_title_configured(&self) -> bool {
        self.window_title_template.is_some()
    }

    pub(crate) fn window_title_uses_terminal_title(&self) -> bool {
        self.window_title_template
            .as_ref()
            .is_some_and(|(template, _)| template.uses(WindowTitleToken::TerminalTitle))
    }

    pub(crate) fn window_title(&self) -> Option<String> {
        let (template, hostname) = self.window_title_template.as_ref()?;
        let workspace = self
            .state
            .active
            .and_then(|ws_idx| self.state.workspaces.get(ws_idx));

        let mut title = String::new();
        for part in template.parts() {
            match part {
                WindowTitlePart::Literal(literal) => title.push_str(literal),
                WindowTitlePart::Token(WindowTitleToken::Hostname) => title.push_str(hostname),
                WindowTitlePart::Token(WindowTitleToken::Workspace) => {
                    if let Some(workspace) = workspace {
                        title.push_str(
                            &workspace.display_name_from_terminals(&self.state.terminals),
                        );
                    }
                }
                WindowTitlePart::Token(WindowTitleToken::Tab) => {
                    if let Some(name) = workspace.and_then(|ws| ws.active_tab_display_name()) {
                        title.push_str(&name);
                    }
                }
                WindowTitlePart::Token(WindowTitleToken::Pane) => {
                    if let Some(label) = self
                        .focused_terminal_state()
                        .and_then(|terminal| terminal.manual_label.as_deref())
                    {
                        title.push_str(label);
                    }
                }
                WindowTitlePart::Token(WindowTitleToken::TerminalTitle) => {
                    if let Some(terminal_title) = self
                        .focused_terminal_state()
                        .and_then(|terminal| terminal.terminal_title_stripped())
                    {
                        title.push_str(&terminal_title);
                    }
                }
            }
        }
        Some(title)
    }

    fn focused_terminal_state(&self) -> Option<&crate::terminal::TerminalState> {
        let workspace = self.state.workspaces.get(self.state.active?)?;
        let terminal_id = workspace.terminal_id(workspace.focused_pane_id()?)?;
        self.state.terminals.get(terminal_id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;
    use crate::workspace::Workspace;

    #[test]
    fn renders_workspace_tab_and_focused_pane_values() {
        let mut app = App::new(
            &Config::default(),
            true,
            None,
            tokio::sync::mpsc::unbounded_channel().1,
            crate::api::EventHub::default(),
        );
        app.state.workspaces = vec![Workspace::test_new("workspace")];
        app.state.active = Some(0);
        app.state.ensure_test_terminals();
        let pane = app.state.workspaces[0].tabs[0].root_pane;
        let terminal_id = app.state.workspaces[0].terminal_id(pane).unwrap().clone();
        app.state
            .terminals
            .get_mut(&terminal_id)
            .unwrap()
            .set_manual_label("shell".into());
        app.configure_window_title("{workspace}/{tab}/{pane}");
        assert_eq!(app.window_title().as_deref(), Some("workspace/1/shell"));
    }
}
