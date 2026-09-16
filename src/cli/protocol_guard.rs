// Modified by the zynk project: this file differs from the upstream version it was derived from.
// See NOTICE ("Modified files (Apache-2.0 provenance)") for the provenance and the license terms.
use std::fmt;

use crate::api::schema::{ErrorBody, ErrorResponse};

#[derive(Debug)]
struct ProtocolMismatch(ErrorResponse);

impl fmt::Display for ProtocolMismatch {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0.error.message)
    }
}

impl std::error::Error for ProtocolMismatch {}

pub(super) fn mismatch_response(
    request_id: &str,
    server_protocol: u32,
    restart_guidance: &str,
) -> Option<ErrorResponse> {
    let client_protocol = crate::protocol::PROTOCOL_VERSION;
    if client_protocol == server_protocol {
        return None;
    }

    let message = if client_protocol > server_protocol {
        format!(
            "client protocol {client_protocol} is newer than server protocol {server_protocol}; restart the zynk server before using this command. {restart_guidance}"
        )
    } else {
        format!(
            "client protocol {client_protocol} is older than server protocol {server_protocol}; upgrade the zynk client before using this command"
        )
    };

    Some(ErrorResponse {
        id: request_id.to_string(),
        error: ErrorBody {
            code: "protocol_mismatch".into(),
            message,
        },
    })
}

pub(super) fn mismatch_error(response: ErrorResponse) -> std::io::Error {
    std::io::Error::other(ProtocolMismatch(response))
}

pub(super) fn error_response(err: &std::io::Error) -> Option<&ErrorResponse> {
    err.get_ref()
        .and_then(|source| source.downcast_ref::<ProtocolMismatch>())
        .map(|mismatch| &mismatch.0)
}
