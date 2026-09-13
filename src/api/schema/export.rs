use serde_json::{json, Value};

use super::{ErrorResponse, EventEnvelope, Request, SubscriptionEventEnvelope, SuccessResponse};

fn protocol_schema_entry<T: schemars::JsonSchema>(name: &str) -> Value {
    let mut schema = schemars::schema_for!(T).to_value();
    rewrite_schema_refs(&mut schema, name);
    schema
}

fn rewrite_schema_refs(value: &mut Value, schema_name: &str) {
    match value {
        Value::Object(object) => {
            if let Some(Value::String(reference)) = object.get_mut("$ref") {
                if let Some(path) = reference.strip_prefix("#/") {
                    *reference = format!("#/schemas/{schema_name}/{path}");
                }
            }
            for child in object.values_mut() {
                rewrite_schema_refs(child, schema_name);
            }
        }
        Value::Array(items) => {
            for item in items {
                rewrite_schema_refs(item, schema_name);
            }
        }
        _ => {}
    }
}

pub(crate) fn protocol_schema_document() -> Value {
    json!({
        "$schema": "https://json-schema.org/draft/2020-12/schema",
        "title": "Zynk API",
        "schema_version": 1,
        "protocol": crate::protocol::PROTOCOL_VERSION,
        "schemas": {
            "request": protocol_schema_entry::<Request>("request"),
            "success_response": protocol_schema_entry::<SuccessResponse>("success_response"),
            "error_response": protocol_schema_entry::<ErrorResponse>("error_response"),
            "event": protocol_schema_entry::<EventEnvelope>("event"),
            "subscription_event": protocol_schema_entry::<SubscriptionEventEnvelope>("subscription_event"),
        },
    })
}
