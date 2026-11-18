//! The gRPC status codes, their names and their details.

use std::sync::OnceLock;

use prost_reflect::{DescriptorPool, DynamicMessage};
use serde::Serialize;
use tonic::{Code, Status};

use crate::message::JsonOptions;

/// The source of the `google.rpc.Status` message.
const STATUS_PROTO: &str = include_str!("../proto/google/rpc/status.proto");
/// The source of the `google.rpc` error details.
const ERROR_DETAILS_PROTO: &str = include_str!("../proto/google/rpc/error_details.proto");

/// The name of a gRPC status code, like `grpc codes.Code.String()`.
pub fn code_name(code: Code) -> &'static str {
    match code {
        Code::Ok => "OK",
        Code::Cancelled => "Canceled",
        Code::Unknown => "Unknown",
        Code::InvalidArgument => "InvalidArgument",
        Code::DeadlineExceeded => "DeadlineExceeded",
        Code::NotFound => "NotFound",
        Code::AlreadyExists => "AlreadyExists",
        Code::PermissionDenied => "PermissionDenied",
        Code::ResourceExhausted => "ResourceExhausted",
        Code::FailedPrecondition => "FailedPrecondition",
        Code::Aborted => "Aborted",
        Code::OutOfRange => "OutOfRange",
        Code::Unimplemented => "Unimplemented",
        Code::Internal => "Internal",
        Code::Unavailable => "Unavailable",
        Code::DataLoss => "DataLoss",
        Code::Unauthenticated => "Unauthenticated",
    }
}

/// The status of a gRPC call, as it is rendered to the user.
#[derive(Debug, Serialize)]
pub struct StatusInfo {
    pub code: String,
    pub number: u32,
    pub message: String,
    /// The details of the status. They are absent when the status has none.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub details: Vec<serde_json::Value>,
}

impl From<&Status> for StatusInfo {
    fn from(status: &Status) -> Self {
        Self {
            code: code_name(status.code()).to_string(),
            number: status.code() as u32,
            message: status.message().to_string(),
            details: status_details(status),
        }
    }
}

/// The details of a status, rendered like the original does.
///
/// The details of a status are the messages of the `google.rpc.Status` that the server
/// sent. Every one of them is rendered with the type of the message, like the original
/// does.
pub fn status_details(status: &Status) -> Vec<serde_json::Value> {
    let raw = status.details();
    if raw.is_empty() {
        return Vec::new();
    }

    let Some(pool) = details_pool() else {
        return Vec::new();
    };
    let Some(desc) = pool.get_message_by_name("google.rpc.Status") else {
        return Vec::new();
    };
    let Ok(message) = DynamicMessage::decode(desc, raw) else {
        return Vec::new();
    };

    let mut details = Vec::new();
    let Some(list) = message
        .get_field_by_name("details")
        .and_then(|v| v.as_list().map(|list| list.to_vec()))
    else {
        return Vec::new();
    };

    for detail in list {
        let Some(any) = detail.as_message() else {
            continue;
        };
        let type_url = any
            .get_field_by_name("type_url")
            .and_then(|v| v.as_str().map(|s| s.to_string()))
            .unwrap_or_default();
        let value = any
            .get_field_by_name("value")
            .and_then(|v| v.as_bytes().map(|bytes| bytes.to_vec()))
            .unwrap_or_default();

        // The name of the message is the last part of the type URL.
        let name = type_url.rsplit('/').next().unwrap_or(&type_url);
        let mut rendered = match pool.get_message_by_name(name) {
            Some(desc) => match DynamicMessage::decode(desc, value.as_slice()) {
                Ok(message) => crate::message::to_json_value(&message, JsonOptions::default())
                    .unwrap_or_default(),
                Err(_) => serde_json::Value::Null,
            },
            None => serde_json::Value::Null,
        };

        if let serde_json::Value::Object(map) = &mut rendered {
            map.insert(
                "@type".to_string(),
                serde_json::Value::String(type_url.clone()),
            );
        }
        crate::message::sort_keys(&mut rendered);
        details.push(rendered);
    }

    details
}

/// The descriptors of the `google.rpc` messages. They are compiled once.
fn details_pool() -> Option<&'static DescriptorPool> {
    static POOL: OnceLock<Option<DescriptorPool>> = OnceLock::new();
    POOL.get_or_init(compile_details_pool).as_ref()
}

/// Compiles the vendored `google.rpc` protos.
fn compile_details_pool() -> Option<DescriptorPool> {
    use protox::Compiler;
    use protox::file::{ChainFileResolver, File, FileResolver, GoogleFileResolver};

    /// A resolver that serves the vendored protos.
    struct Vendored;

    impl FileResolver for Vendored {
        fn open_file(&self, name: &str) -> Result<File, protox::Error> {
            let source = match name {
                "google/rpc/status.proto" => STATUS_PROTO,
                "google/rpc/error_details.proto" => ERROR_DETAILS_PROTO,
                _ => return Err(protox::Error::file_not_found(name)),
            };
            File::from_source(name, source)
        }
    }

    let mut resolver = ChainFileResolver::new();
    resolver.add(Vendored);
    resolver.add(GoogleFileResolver::new());

    let mut compiler = Compiler::with_file_resolver(resolver);
    // The imported files are needed to build the pool.
    compiler.include_imports(true);
    compiler.open_file("google/rpc/status.proto").ok()?;
    compiler.open_file("google/rpc/error_details.proto").ok()?;
    let fds = compiler.file_descriptor_set();
    DescriptorPool::from_file_descriptor_set(fds).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_match_grpc() {
        assert_eq!(code_name(Code::Ok), "OK");
        assert_eq!(code_name(Code::Cancelled), "Canceled");
        assert_eq!(code_name(Code::Unauthenticated), "Unauthenticated");
    }

    #[test]
    fn a_status_without_details_has_none() {
        let status = Status::internal("internal error");
        assert!(status_details(&status).is_empty());
    }

    #[test]
    fn compiles_the_details_of_the_status() {
        let pool = details_pool().expect("the google.rpc protos are compiled");
        assert!(pool.get_message_by_name("google.rpc.Status").is_some());
        assert!(pool.get_message_by_name("google.rpc.BadRequest").is_some());
    }
}
