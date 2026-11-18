//! Response formatters that mirror `format/curl` and `format/json` of the original.

use std::io::Write;

use anyhow::Result;
use tonic::Status;
use tonic::metadata::{KeyAndValueRef, MetadataMap};

use crate::message::{self, DynamicMessage, JsonOptions};
use crate::status::StatusInfo;

/// The result of a gRPC call.
#[derive(Debug, Clone)]
pub struct Response {
    pub headers: MetadataMap,
    pub messages: Vec<DynamicMessage>,
    pub trailers: MetadataMap,
    pub status: Status,
}

/// Formats the response like the `curl` format of the original: the headers, the messages,
/// the trailers and the status, separated by empty lines.
///
/// The headers, the trailers and the status are rendered only when `enrich` is enabled,
/// like the original does.
pub fn format_curl(response: &Response, enrich: bool, emit_defaults: bool) -> Result<String> {
    let mut out: Vec<u8> = Vec::new();
    {
        let mut sink = CurlSink::new(&mut out, enrich, emit_defaults);
        sink.header(&response.headers)?;
        for msg in &response.messages {
            sink.message(msg)?;
        }
        sink.trailer(&response.trailers)?;
        sink.status(&response.status)?;
    }
    Ok(String::from_utf8_lossy(&out).into_owned())
}

/// A sink that renders the parts of a response as they arrive.
pub trait ResponseSink {
    /// Renders the headers of the response.
    fn header(&mut self, headers: &MetadataMap) -> Result<()>;
    /// Renders a message of the response.
    fn message(&mut self, message: &DynamicMessage) -> Result<()>;
    /// Renders the trailers of the response.
    fn trailer(&mut self, trailers: &MetadataMap) -> Result<()>;
    /// Renders the status of the response.
    fn status(&mut self, status: &Status) -> Result<()>;
}

/// A sink that renders a response like the `curl` format of the original.
pub struct CurlSink<W> {
    out: W,
    enrich: bool,
    emit_defaults: bool,
    wrote_header: bool,
    wrote_message: bool,
    wrote_trailer: bool,
}

impl<W: Write> CurlSink<W> {
    /// Creates a new sink.
    pub fn new(out: W, enrich: bool, emit_defaults: bool) -> Self {
        Self {
            out,
            enrich,
            emit_defaults,
            wrote_header: false,
            wrote_message: false,
            wrote_trailer: false,
        }
    }
}

impl<W: Write> ResponseSink for CurlSink<W> {
    fn header(&mut self, headers: &MetadataMap) -> Result<()> {
        if !self.enrich {
            return Ok(());
        }
        let headers = format_metadata(headers);
        if headers.is_empty() {
            return Ok(());
        }
        writeln!(self.out, "{headers}")?;
        self.wrote_header = true;
        Ok(())
    }

    fn message(&mut self, message: &DynamicMessage) -> Result<()> {
        if self.wrote_header {
            writeln!(self.out)?;
        }
        let options = JsonOptions {
            emit_defaults: self.emit_defaults,
        };
        writeln!(self.out, "{}", message::to_json_pretty(message, options)?)?;
        self.wrote_message = true;
        Ok(())
    }

    fn trailer(&mut self, trailers: &MetadataMap) -> Result<()> {
        if !self.enrich {
            return Ok(());
        }
        let trailers = format_metadata(trailers);
        if trailers.is_empty() {
            return Ok(());
        }
        if self.wrote_header || self.wrote_message {
            writeln!(self.out)?;
        }
        writeln!(self.out, "{trailers}")?;
        self.wrote_trailer = true;
        Ok(())
    }

    fn status(&mut self, status: &Status) -> Result<()> {
        if !self.enrich {
            return Ok(());
        }
        if self.wrote_header || self.wrote_message || self.wrote_trailer {
            writeln!(self.out)?;
        }
        let info = StatusInfo::from(status);
        write!(
            self.out,
            "code: {}\nnumber: {}\nmessage: {:?}\n",
            info.code, info.number, info.message
        )?;
        if !info.details.is_empty() {
            writeln!(self.out, "details: ")?;
            for detail in &info.details {
                writeln!(self.out, "  {}", format_detail(detail))?;
            }
        }
        // A failed call is followed by an empty line, like the original does.
        if status.code() != tonic::Code::Ok {
            writeln!(self.out)?;
        }
        Ok(())
    }
}

/// Formats a detail of a status, like the original does.
///
/// The JSON is rendered without the indentation and with a space after the commas.
fn format_detail(detail: &serde_json::Value) -> String {
    let mut buf = Vec::new();
    let formatter = serde_json::ser::PrettyFormatter::with_indent(b"");
    let mut ser = serde_json::Serializer::with_formatter(&mut buf, formatter);
    if serde::Serialize::serialize(detail, &mut ser).is_err() {
        return String::new();
    }
    String::from_utf8_lossy(&buf)
        .replace('\n', "")
        .replace(',', ", ")
}

/// Formats the response like the `json` format of the original.
///
/// The headers and the trailers are rendered only when `enrich` is enabled, like the
/// original does.
pub fn format_json_response(
    response: &Response,
    enrich: bool,
    emit_defaults: bool,
) -> Result<String> {
    let options = JsonOptions { emit_defaults };
    let mut messages = Vec::with_capacity(response.messages.len());
    for msg in &response.messages {
        messages.push(message::to_json_value(msg, options)?);
    }

    // The keys are inserted in the order of the original.
    let mut value = serde_json::Map::new();
    value.insert(
        "status".to_string(),
        serde_json::to_value(StatusInfo::from(&response.status))?,
    );
    if enrich {
        value.insert("header".to_string(), metadata_to_value(&response.headers));
    }
    if !messages.is_empty() {
        value.insert("messages".to_string(), serde_json::Value::Array(messages));
    }
    if enrich {
        value.insert("trailer".to_string(), metadata_to_value(&response.trailers));
    }

    let mut out = serde_json::to_string_pretty(&serde_json::Value::Object(value))?;
    out.push('\n');
    Ok(out)
}

/// Formats metadata as `key: value` lines, sorted like the original does.
fn format_metadata(metadata: &MetadataMap) -> String {
    let mut lines: Vec<String> = Vec::new();
    for entry in metadata.iter() {
        let (key, value) = match entry {
            KeyAndValueRef::Ascii(key, value) => {
                (key.as_str(), value.to_str().unwrap_or_default().to_string())
            }
            KeyAndValueRef::Binary(key, _) => (key.as_str(), String::new()),
        };
        lines.push(format!("{key}: {value}"));
    }
    lines.sort();
    lines.join("\n")
}

fn metadata_to_value(metadata: &MetadataMap) -> serde_json::Value {
    let mut map: serde_json::Map<String, serde_json::Value> = serde_json::Map::new();
    for entry in metadata.iter() {
        let (key, value) = match entry {
            KeyAndValueRef::Ascii(key, value) => {
                (key.as_str(), value.to_str().unwrap_or_default().to_string())
            }
            KeyAndValueRef::Binary(key, _) => (key.as_str(), String::new()),
        };
        let values = map
            .entry(key.to_string())
            .or_insert_with(|| serde_json::Value::Array(Vec::new()));
        if let serde_json::Value::Array(values) = values {
            values.push(serde_json::Value::String(value));
        }
    }
    serde_json::Value::Object(map)
}

#[cfg(test)]
mod tests {
    use super::*;

    use prost_reflect::{DescriptorPool, DynamicMessage};

    fn pool() -> DescriptorPool {
        let fds = protox::compile(["testdata/proto/test.proto"], ["testdata/proto"]).unwrap();
        DescriptorPool::from_file_descriptor_set(fds).unwrap()
    }

    fn response() -> Response {
        let desc = pool().get_message_by_name("test.UnaryResponse").unwrap();
        let mut msg = DynamicMessage::new(desc);
        msg.set_field_by_name("message", prost_reflect::Value::String("hello".to_string()));

        let mut headers = MetadataMap::new();
        headers.insert("content-type", "application/grpc".parse().unwrap());
        let mut trailers = MetadataMap::new();
        trailers.insert("trailer-key", "trailer-value".parse().unwrap());

        Response {
            headers,
            messages: vec![msg],
            trailers,
            status: Status::ok(""),
        }
    }

    #[test]
    fn formats_curl_like_output() {
        let out = format_curl(&response(), true, false).unwrap();
        assert_eq!(
            out,
            "content-type: application/grpc\n\
             \n\
             {\n  \"message\": \"hello\"\n}\n\
             \n\
             trailer-key: trailer-value\n\
             \n\
             code: OK\nnumber: 0\nmessage: \"\"\n"
        );
    }

    #[test]
    fn formats_curl_like_output_without_enrichment() {
        let out = format_curl(&response(), false, false).unwrap();
        assert_eq!(out, "{\n  \"message\": \"hello\"\n}\n");
    }

    #[test]
    fn formats_json_output_with_the_keys_of_the_original() {
        let out = format_json_response(&response(), true, false).unwrap();
        // The order of the keys is the one of the original.
        let status = out.find("\"status\"").expect("the status");
        let header = out.find("\"header\"").expect("the header");
        let messages = out.find("\"messages\"").expect("the messages");
        let trailer = out.find("\"trailer\"").expect("the trailer");
        assert!(status < header, "{out}");
        assert!(header < messages, "{out}");
        assert!(messages < trailer, "{out}");
    }

    #[test]
    fn omits_the_messages_of_a_failed_call() {
        let mut response = response();
        response.messages.clear();
        let out = format_json_response(&response, true, false).unwrap();
        assert!(!out.contains("\"messages\""), "{out}");
    }

    #[test]
    fn formats_json_output() {
        let out = format_json_response(&response(), true, false).unwrap();
        let value: serde_json::Value = serde_json::from_str(&out).unwrap();
        assert_eq!(value["status"]["code"], "OK");
        assert_eq!(value["header"]["content-type"][0], "application/grpc");
        assert_eq!(value["messages"][0]["message"], "hello");
        assert_eq!(value["trailer"]["trailer-key"][0], "trailer-value");
    }

    #[test]
    fn formats_json_output_without_enrichment() {
        let out = format_json_response(&response(), false, false).unwrap();
        let value: serde_json::Value = serde_json::from_str(&out).unwrap();
        assert!(value.get("header").is_none());
        assert!(value.get("trailer").is_none());
        assert_eq!(value["messages"][0]["message"], "hello");
    }
}
