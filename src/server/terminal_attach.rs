// Modified by the zynk project: this file differs from the upstream version it was derived from.
// See NOTICE ("Modified files (Apache-2.0 provenance)") for the provenance and the license terms.
pub(crate) fn paste_payload_for_runtime(
    runtime: &crate::terminal::TerminalRuntime,
    text: &str,
) -> String {
    if runtime.bracketed_paste_enabled() {
        format!("\x1b[200~{text}\x1b[201~")
    } else {
        text.to_owned()
    }
}
