// Modified by the zynk project: this file differs from the upstream version it was derived from.
// See NOTICE ("Modified files (Apache-2.0 provenance)") for the provenance and the license terms.
use std::fmt;
use std::path::Path;

use crate::api::schema::{ErrorBody, ErrorResponse};

#[derive(Debug)]
pub(super) struct ServerNotRunningReported {
    pub(super) response: ErrorResponse,
}

impl fmt::Display for ServerNotRunningReported {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.response.error.message)
    }
}

impl std::error::Error for ServerNotRunningReported {}

pub(super) fn response(request_id: &str, socket_path: &Path) -> ErrorResponse {
    ErrorResponse {
        id: request_id.to_string(),
        error: ErrorBody {
            code: "server_not_running".into(),
            message: format!(
                "no zynk server is running at {}; run `{}` to start or attach it",
                socket_path.display(),
                startup_command(socket_path)
            ),
        },
    }
}

fn startup_command(socket_path: &Path) -> String {
    let session_socket =
        crate::session::api_socket_path_for(crate::session::active_name().as_deref());
    if socket_path == session_socket {
        crate::session::local_attach_command()
    } else {
        "zynk".to_string()
    }
}

pub(super) fn reported_error(response: ErrorResponse) -> std::io::Error {
    std::io::Error::other(ServerNotRunningReported { response })
}

pub(super) fn reported_response(err: &std::io::Error) -> Option<&ErrorResponse> {
    err.get_ref()
        .and_then(|source| source.downcast_ref::<ServerNotRunningReported>())
        .map(|reported| &reported.response)
}
