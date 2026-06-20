//! zynk fork: runtime namespace helpers (ADR 0003).

use std::path::PathBuf;

use crate::zynk::message::new_prefixed_id;

pub const RUNTIME_ID_FILE: &str = "runtime.id";

pub fn runtime_id_path() -> PathBuf {
    crate::session::active_api_socket_path()
        .parent()
        .map(|parent| parent.join(RUNTIME_ID_FILE))
        .unwrap_or_else(|| crate::session::data_dir().join(RUNTIME_ID_FILE))
}

pub fn socket_namespace() -> String {
    crate::session::active_api_socket_path()
        .to_string_lossy()
        .to_string()
}

pub fn ensure_runtime_id_file() -> std::io::Result<String> {
    let path = runtime_id_path();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let id = new_prefixed_id("rt");
    std::fs::write(&path, format!("{id}\n"))?;
    Ok(id)
}

pub fn read_runtime_id() -> Result<String, String> {
    let path = runtime_id_path();
    let value = std::fs::read_to_string(&path).map_err(|err| {
        format!(
            "runtime_identity_missing: could not read {}: {err}",
            path.display()
        )
    })?;
    let id = value.trim();
    if id.is_empty() {
        return Err(format!(
            "runtime_identity_missing: {} is empty",
            path.display()
        ));
    }
    Ok(id.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn socket_namespace_is_a_path_string() {
        let ns = socket_namespace();
        assert!(!ns.is_empty());
    }
}
