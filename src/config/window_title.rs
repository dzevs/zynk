// Modified by the zynk project: this file differs from the upstream version it was derived from.
// See NOTICE ("Modified files (Apache-2.0 provenance)") for the provenance and the license terms.
/// Maximum characters in a rendered outer terminal window title.
pub(crate) const MAX_WINDOW_TITLE_CHARS: usize = 200;

pub(crate) fn default_window_title() -> String {
    "{hostname}: {workspace}".to_string()
}

pub(crate) fn sanitize_window_title_text(value: &str) -> Option<String> {
    let sanitized = value
        .chars()
        .filter(|ch| !matches!(*ch, '\u{1b}' | '\u{7}' | '\u{9c}') && !ch.is_control())
        .take(MAX_WINDOW_TITLE_CHARS)
        .collect::<String>()
        .trim()
        .to_string();
    (!sanitized.is_empty()).then_some(sanitized)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WindowTitleToken {
    Hostname,
    Workspace,
    Tab,
    Pane,
    TerminalTitle,
}

impl WindowTitleToken {
    fn parse(name: &str) -> Option<Self> {
        match name {
            "hostname" => Some(Self::Hostname),
            "workspace" => Some(Self::Workspace),
            "tab" => Some(Self::Tab),
            "pane" => Some(Self::Pane),
            "terminal_title" => Some(Self::TerminalTitle),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WindowTitlePart {
    Literal(String),
    Token(WindowTitleToken),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WindowTitleTemplate {
    parts: Vec<WindowTitlePart>,
}

impl WindowTitleTemplate {
    /// An empty format disables server-managed outer window titles.
    pub fn parse(template: &str) -> Result<Option<Self>, String> {
        if template.is_empty() {
            return Ok(None);
        }

        let mut parts = Vec::new();
        let mut literal = String::new();
        let mut chars = template.chars().peekable();
        while let Some(ch) = chars.next() {
            match ch {
                '{' if chars.peek() == Some(&'{') => {
                    chars.next();
                    literal.push('{');
                }
                '}' if chars.peek() == Some(&'}') => {
                    chars.next();
                    literal.push('}');
                }
                '{' => {
                    let mut name = String::new();
                    let mut closed = false;
                    for ch in chars.by_ref() {
                        if ch == '}' {
                            closed = true;
                            break;
                        }
                        name.push(ch);
                    }
                    if !closed {
                        return Err("has an unclosed '{'".to_string());
                    }
                    let Some(token) = WindowTitleToken::parse(name.trim()) else {
                        return Err(format!("has unknown token '{{{name}}}'"));
                    };
                    if !literal.is_empty() {
                        parts.push(WindowTitlePart::Literal(std::mem::take(&mut literal)));
                    }
                    parts.push(WindowTitlePart::Token(token));
                }
                '}' => return Err("has an unmatched '}'".to_string()),
                _ => literal.push(ch),
            }
        }
        if !literal.is_empty() {
            parts.push(WindowTitlePart::Literal(literal));
        }

        Ok((!parts.is_empty()).then_some(Self { parts }))
    }

    pub fn parts(&self) -> &[WindowTitlePart] {
        &self.parts
    }

    pub fn uses(&self, token: WindowTitleToken) -> bool {
        self.parts
            .iter()
            .any(|part| matches!(part, WindowTitlePart::Token(other) if *other == token))
    }
}

pub(crate) fn window_title_diagnostics(template: &str) -> Option<String> {
    WindowTitleTemplate::parse(template)
        .err()
        .map(|err| format!("ui.window_title {err}; leaving the outer terminal title alone"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_tokens_literals_and_escapes() {
        let template = WindowTitleTemplate::parse("{hostname}: {workspace} {{a}}")
            .unwrap()
            .unwrap();
        assert_eq!(
            template.parts(),
            [
                WindowTitlePart::Token(WindowTitleToken::Hostname),
                WindowTitlePart::Literal(": ".into()),
                WindowTitlePart::Token(WindowTitleToken::Workspace),
                WindowTitlePart::Literal(" {a}".into()),
            ]
        );
    }

    #[test]
    fn invalid_templates_and_unsafe_titles_are_rejected_or_sanitized() {
        assert!(window_title_diagnostics("{hostname").is_some());
        assert!(window_title_diagnostics("{unknown}").is_some());
        assert_eq!(
            sanitize_window_title_text("  zynk\u{1b} api\u{7}\n  ").as_deref(),
            Some("zynk api")
        );
        assert_eq!(sanitize_window_title_text("\u{7}\n"), None);
    }
}
