//! zynk fork (M3a): `zynk.message_received` API handler.
//!
//! Resolves the AUTHORITATIVE receiver identity from live hook authority (never
//! the detection fallback) and delegates the DB write to the App-owned receipt
//! worker. The handler stays synchronous and never opens a nested Tokio runtime.

use super::responses::{encode_error, encode_success};
use crate::api::schema::{ResponseResult, ZynkMessageReceivedParams};
use crate::app::App;
use crate::zynk::receipt::ReceiptRequest;
use crate::zynk::receipt_worker::DEFAULT_RECEIPT_TIMEOUT;

impl App {
    pub(crate) fn handle_zynk_message_received(
        &mut self,
        id: String,
        params: ZynkMessageReceivedParams,
    ) -> String {
        // Param validation: `receiver_seq`, if present, must be positive (plan D2).
        if matches!(params.receiver_seq, Some(seq) if seq <= 0) {
            return encode_error(
                id,
                "invalid_params",
                "receiver_seq must be positive when present",
            );
        }

        // Authoritative (hook-derived, never detection) receiver identity. A
        // detection-only or non-agent pane is NOT receipt-capable.
        let Some(receiver) = self.authoritative_receiver_identity(&params.pane_id) else {
            return encode_error(
                id,
                "receiver_identity_unverified",
                "pane has no hook-authoritative agent identity; not receipt-capable",
            );
        };

        // The receipt worker is installed by the headless server at startup; it is
        // absent in CLI / unit-test App contexts.
        let Some(worker) = self.zynk_receipt_worker.as_ref() else {
            return encode_error(
                id,
                "receipt_worker_unavailable",
                "receipt worker is not available in this runtime",
            );
        };

        let request = ReceiptRequest {
            message_id: params.message_id,
            conversation_id: params.conversation_id,
            conversation_seq: params.conversation_seq,
            runtime_session_id: params.runtime_session_id,
            socket_namespace: params.socket_namespace,
            receiver_seq: params.receiver_seq,
            timestamp: params.timestamp,
            status: params.status,
            receiver_agent_session_hint: params.receiver_agent_session,
        };

        let current_socket_namespace = crate::zynk::runtime::socket_namespace();
        let current_runtime_id = crate::zynk::runtime::read_runtime_id().unwrap_or_default();
        let now = crate::zynk::message::now_rfc3339();

        match worker.submit(
            request,
            receiver,
            current_socket_namespace,
            current_runtime_id,
            now,
            DEFAULT_RECEIPT_TIMEOUT,
        ) {
            Ok(accepted) => encode_success(
                id,
                ResponseResult::ZynkMessageReceived {
                    message_id: accepted.message_id,
                    conversation_id: accepted.conversation_id,
                    conversation_seq: accepted.conversation_seq,
                    receipt_status: accepted.status.as_str().to_string(),
                    delivery_status: "received".to_string(),
                    receiver_pane_id: accepted.receiver_pane_id,
                    receiver_agent_label: accepted.receiver_agent_label,
                    next: "receipt recorded; processed is deferred".to_string(),
                },
            ),
            Err(err) => encode_error(id, err.code, err.message),
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::api::schema::{Method, Request, ZynkMessageReceivedParams};

    #[test]
    fn message_received_does_not_request_ui_changes() {
        let request = Request {
            id: "t".into(),
            method: Method::ZynkMessageReceived(ZynkMessageReceivedParams {
                pane_id: "w-1".into(),
                message_id: "msg".into(),
                conversation_id: "conv".into(),
                conversation_seq: 1,
                runtime_session_id: "rt".into(),
                socket_namespace: "sock".into(),
                receiver_seq: None,
                timestamp: None,
                status: None,
                receiver_agent_session: None,
            }),
        };
        assert!(!crate::api::request_changes_ui(&request));
    }
}
