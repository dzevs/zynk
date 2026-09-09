//! Build identity helpers.

pub const BASE_VERSION: &str = env!("CARGO_PKG_VERSION");

pub fn channel() -> &'static str {
    non_empty(option_env!("ZYNK_BUILD_CHANNEL")).unwrap_or("stable")
}

pub fn build_id() -> Option<&'static str> {
    non_empty(option_env!("ZYNK_BUILD_ID"))
}

pub fn version() -> String {
    match channel() {
        "stable" => BASE_VERSION.to_string(),
        channel => match build_id() {
            Some(build_id) => format!("{BASE_VERSION}-{channel}.{build_id}"),
            None => format!("{BASE_VERSION}-{channel}"),
        },
    }
}

/// The source commit this binary was built from, when the build could attest one (ADR 0013
/// custody). `None` for a build with no git checkout — a crates.io `.crate` unpack or a source
/// tarball — in which case the remote-copy install path refuses to seed a remote from this binary.
pub fn build_sha() -> Option<&'static str> {
    non_empty(option_env!("ZYNK_BUILD_SHA"))
}

/// The `--version` line: `zynk <version>`, plus the attested source commit when there is one.
pub fn version_line() -> String {
    match build_sha() {
        Some(sha) => format!("zynk {} ({sha})", version()),
        None => format!("zynk {}", version()),
    }
}

pub fn is_preview() -> bool {
    channel() == "preview"
}

fn non_empty(value: Option<&'static str>) -> Option<&'static str> {
    value.and_then(|value| {
        let trimmed = value.trim();
        if trimmed.is_empty() {
            None
        } else {
            Some(trimmed)
        }
    })
}

#[cfg(test)]
mod tests {
    #[test]
    fn stable_version_defaults_to_cargo_version() {
        assert!(!super::version().is_empty());
    }
}
