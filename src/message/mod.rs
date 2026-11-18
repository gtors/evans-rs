pub mod filler;

use anyhow::{Context, Result, anyhow};
use prost_reflect::{DeserializeOptions, MessageDescriptor, SerializeOptions};

pub use prost_reflect::DynamicMessage;

/// Options that control how a dynamic message is encoded to JSON.
#[derive(Debug, Clone, Copy, Default)]
pub struct JsonOptions {
    /// Render fields with their default values.
    pub emit_defaults: bool,
}

/// Decodes a JSON request body into a dynamic message.
///
/// The JSON mapping is the canonical protobuf JSON mapping: field names may be given
/// in either the JSON (camelCase) or the proto (snake_case) form, enums accept their
/// name or number, 64-bit integers accept a number or a decimal string and bytes are
/// base64-encoded. Unknown fields are rejected, like `protojson.UnmarshalOptions`.
///
/// Only the first JSON document of the body is decoded, like the original does with its
/// JSON decoder.
pub fn from_json(desc: &MessageDescriptor, body: &str) -> Result<DynamicMessage> {
    from_json_documents(desc, body)?
        .into_iter()
        .next()
        .ok_or_else(|| anyhow!("failed to decode JSON request: the request body is empty"))
}

/// Decodes all JSON documents of the body into dynamic messages.
///
/// Client streaming RPCs accept a stream of JSON documents, one per request.
pub fn from_json_documents(desc: &MessageDescriptor, body: &str) -> Result<Vec<DynamicMessage>> {
    let mut messages = Vec::new();
    for value in serde_json::Deserializer::from_str(body).into_iter::<serde_json::Value>() {
        let value = value.context("failed to decode JSON request")?;
        let raw = serde_json::to_string(&value).context("failed to decode JSON request")?;
        messages.push(decode_json_value(desc, &raw)?);
    }
    Ok(messages)
}

fn decode_json_value(desc: &MessageDescriptor, body: &str) -> Result<DynamicMessage> {
    let mut de = serde_json::Deserializer::from_str(body);
    let options = DeserializeOptions::new().deny_unknown_fields(true);
    let msg = DynamicMessage::deserialize_with_options(desc.clone(), &mut de, &options)
        .context("failed to decode JSON request")?;
    Ok(msg)
}

/// Encodes a dynamic message into JSON.
pub fn to_json(msg: &DynamicMessage, options: JsonOptions) -> Result<String> {
    let opts = SerializeOptions::new().skip_default_fields(!options.emit_defaults);
    let mut buf = Vec::new();
    let mut ser = serde_json::Serializer::new(&mut buf);
    msg.serialize_with_options(&mut ser, &opts)
        .context("failed to encode JSON response")?;
    String::from_utf8(buf).context("failed to encode JSON response")
}

/// Encodes a dynamic message into JSON, pretty-printed with two-space indentation.
pub fn to_json_pretty(msg: &DynamicMessage, options: JsonOptions) -> Result<String> {
    let value = to_json_value(msg, options)?;
    serde_json::to_string_pretty(&value).context("failed to encode JSON response")
}

/// Encodes a dynamic message into a JSON value.
pub fn to_json_value(msg: &DynamicMessage, options: JsonOptions) -> Result<serde_json::Value> {
    let opts = SerializeOptions::new().skip_default_fields(!options.emit_defaults);
    let mut value =
        serde_json::to_value(msg.serialize_with_options(serde_json::value::Serializer, &opts)?)
            .context("failed to encode JSON response")?;
    sort_keys(&mut value);
    Ok(value)
}

/// Sorts the keys of the objects of the value.
///
/// The original converts the messages to maps before they are marshaled, so the fields
/// of a message are sorted by their names instead of their numbers.
pub fn sort_keys(value: &mut serde_json::Value) {
    match value {
        serde_json::Value::Object(map) => {
            let mut entries: Vec<(String, serde_json::Value)> =
                std::mem::take(map).into_iter().collect();
            entries.sort_by(|a, b| a.0.cmp(&b.0));
            for (_, entry) in &mut entries {
                sort_keys(entry);
            }
            *map = entries.into_iter().collect();
        }
        serde_json::Value::Array(items) => {
            for item in items {
                sort_keys(item);
            }
        }
        _ => {}
    }
}

/// Encodes a dynamic message into the protobuf binary wire format.
pub fn to_binary(msg: &DynamicMessage) -> Result<Vec<u8>> {
    let mut buf = Vec::new();
    prost_reflect::prost::Message::encode(msg, &mut buf).context("failed to encode message")?;
    Ok(buf)
}

/// Decodes a message from the protobuf binary wire format.
pub fn from_binary(desc: &MessageDescriptor, body: &[u8]) -> Result<DynamicMessage> {
    DynamicMessage::decode(desc.clone(), body).context("failed to decode message")
}

#[cfg(test)]
mod tests {
    use super::*;

    use prost_reflect::{DescriptorPool, Value};

    fn pool() -> DescriptorPool {
        let fds = protox::compile(["testdata/proto/test.proto"], ["testdata/proto"]).unwrap();
        DescriptorPool::from_file_descriptor_set(fds).unwrap()
    }

    fn request_desc() -> MessageDescriptor {
        pool()
            .get_message_by_name("test.UnaryRequest")
            .expect("test.UnaryRequest should exist")
    }

    #[test]
    fn builds_an_empty_message() {
        let msg = DynamicMessage::new(request_desc());
        assert!(!msg.has_field_by_name("name"));
        assert!(!msg.has_field_by_name("suit"));
    }

    #[test]
    fn decodes_json_with_canonical_names() {
        let msg = from_json(
            &request_desc(),
            r#"{"name": "ktr", "suit": "CLUBS", "age": 20, "tags": ["a", "b"]}"#,
        )
        .unwrap();
        assert_eq!(msg.get_field_by_name("name").unwrap().as_str(), Some("ktr"));
        assert_eq!(
            msg.get_field_by_name("suit").unwrap().as_enum_number(),
            Some(1)
        );
        assert_eq!(msg.get_field_by_name("age").unwrap().as_i32(), Some(20));
        let tags = msg.get_field_by_name("tags").unwrap();
        let list = tags.as_list().unwrap();
        assert_eq!(list.len(), 2);
        assert_eq!(list[0].as_str(), Some("a"));
    }

    #[test]
    fn decodes_json_with_proto_names_and_numeric_enums() {
        let msg = from_json(&request_desc(), r#"{"name": "ktr", "suit": 2}"#).unwrap();
        assert_eq!(
            msg.get_field_by_name("suit").unwrap().as_enum_number(),
            Some(2)
        );
    }

    #[test]
    fn rejects_unknown_fields() {
        let err = from_json(&request_desc(), r#"{"unknown": true}"#).unwrap_err();
        assert!(
            format!("{err:#}").contains("unrecognized field name 'unknown'"),
            "unexpected error: {err:#}"
        );
    }

    #[test]
    fn rejects_invalid_json() {
        let err = from_json(&request_desc(), "{").unwrap_err();
        assert!(err.to_string().contains("failed to decode JSON request"));
    }

    #[test]
    fn encodes_json_without_defaults() {
        let mut msg = DynamicMessage::new(request_desc());
        msg.set_field_by_name("name", Value::String("ktr".to_string()));
        let json = to_json(&msg, JsonOptions::default()).unwrap();
        assert_eq!(json, r#"{"name":"ktr"}"#);
    }

    #[test]
    fn encodes_json_with_defaults() {
        let msg = DynamicMessage::new(request_desc());
        let json = to_json(
            &msg,
            JsonOptions {
                emit_defaults: true,
            },
        )
        .unwrap();
        assert!(json.contains(r#""name":"""#));
        assert!(json.contains(r#""suit":"SUIT_UNSPECIFIED""#));
    }

    #[test]
    fn round_trips_binary() {
        let mut msg = DynamicMessage::new(request_desc());
        msg.set_field_by_name("name", Value::String("ktr".to_string()));
        let bytes = to_binary(&msg).unwrap();
        let decoded = from_binary(&request_desc(), &bytes).unwrap();
        assert_eq!(
            decoded.get_field_by_name("name").unwrap().as_str(),
            Some("ktr")
        );
    }
}
