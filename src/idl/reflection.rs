use std::collections::BTreeMap;

use anyhow::{Context, Result, anyhow, bail};
use futures::StreamExt;
use prost_reflect::prost::Message as _;
use prost_reflect::{DescriptorPool, prost_types::FileDescriptorProto};
use tonic::metadata::MetadataMap;
use tonic::transport::Channel;
use tonic_reflection::pb::v1::server_reflection_request::MessageRequest as V1MessageRequest;
use tonic_reflection::pb::v1::{
    ServerReflectionRequest, server_reflection_client::ServerReflectionClient as V1Client,
};
use tonic_reflection::pb::v1alpha::server_reflection_request::MessageRequest as V1AlphaMessageRequest;
use tonic_reflection::pb::v1alpha::{
    ServerReflectionRequest as V1AlphaServerReflectionRequest,
    server_reflection_client::ServerReflectionClient as V1AlphaClient,
};

use crate::grpc::Headers;

use super::{Error as IdlError, REFLECTION_SERVICE_V1, REFLECTION_SERVICE_V1ALPHA};

pub async fn load(
    channel: Channel,
    headers: &BTreeMap<String, Vec<String>>,
) -> Result<DescriptorPool> {
    let loader = Loader {
        channel,
        metadata: Headers::new(headers.clone()).to_metadata()?,
    };
    loader.load().await
}

struct Loader {
    channel: Channel,
    metadata: MetadataMap,
}

#[derive(Default)]
struct Files {
    ordered: Vec<FileDescriptorProto>,
    seen: Vec<String>,
}

impl Files {
    fn contains(&self, name: &str) -> bool {
        self.seen.iter().any(|n| n == name)
    }
}

impl Loader {
    async fn load(&self) -> Result<DescriptorPool> {
        let services = self.list_services().await?;

        let mut files = Files::default();
        for svc in services {
            let fds = self.file_containing_symbol(&svc).await?;
            for fd in fds {
                self.ingest(&mut files, fd).await?;
            }
        }

        let mut pool = DescriptorPool::new();
        pool.add_file_descriptor_protos(files.ordered)
            .map_err(|e| anyhow!("failed to build descriptor pool: {e}"))?;
        Ok(pool)
    }

    async fn ingest(&self, files: &mut Files, fd: FileDescriptorProto) -> Result<()> {
        let name = fd.name.clone().unwrap_or_default();
        if files.contains(&name) {
            return Ok(());
        }
        for dep in &fd.dependency {
            if files.contains(dep) {
                continue;
            }
            let deps = self.file_by_filename(dep).await?;
            for d in deps {
                Box::pin(self.ingest(files, d)).await?;
            }
        }
        files.seen.push(name);
        files.ordered.push(fd);
        Ok(())
    }

    async fn list_services(&self) -> Result<Vec<String>> {
        match self
            .call(V1MessageRequest::ListServices(String::new()))
            .await?
        {
            Reply::Services(names) => Ok(names
                .into_iter()
                .filter(|name| !is_reflection_service(name))
                .collect()),
            _ => bail!("unexpected response type for list_services request"),
        }
    }

    async fn file_containing_symbol(&self, symbol: &str) -> Result<Vec<FileDescriptorProto>> {
        match self
            .call(V1MessageRequest::FileContainingSymbol(symbol.to_string()))
            .await
        {
            Ok(Reply::Files(files)) => decode_files(files),
            Ok(_) => bail!("unexpected response type for file request"),
            Err(e) if matches!(e.downcast_ref::<IdlError>(), Some(IdlError::UnknownSymbol)) => Err(
                anyhow!("failed to find file containing symbol {symbol:?}: symbol not found"),
            ),
            Err(e) => Err(e.context(format!("failed to find file containing symbol {symbol:?}"))),
        }
    }

    async fn file_by_filename(&self, filename: &str) -> Result<Vec<FileDescriptorProto>> {
        let resp = self
            .call(V1MessageRequest::FileByFilename(filename.to_string()))
            .await
            .context(format!("failed to find file {filename:?}"))?;
        match resp {
            Reply::Files(files) => decode_files(files),
            _ => bail!("unexpected response type for file request"),
        }
    }

    async fn call(&self, request: V1MessageRequest) -> Result<Reply> {
        match self.call_v1(request.clone()).await {
            Ok(resp) => Ok(resp),
            Err(e) if is_unimplemented(&e) => self.call_v1alpha(request).await,
            Err(e) => Err(e),
        }
    }

    async fn call_v1(&self, request: V1MessageRequest) -> Result<Reply> {
        let req = ServerReflectionRequest {
            host: String::new(),
            message_request: Some(request),
        };
        let stream = futures::stream::iter(vec![req]);
        let mut request = tonic::Request::new(stream);
        *request.metadata_mut() = self.metadata.clone();

        let mut client = V1Client::new(self.channel.clone());
        let mut stream = client
            .server_reflection_info(request)
            .await
            .map(|r| r.into_inner())
            .context("failed to call gRPC server reflection")?;
        let resp = stream
            .next()
            .await
            .ok_or_else(|| anyhow!("gRPC server reflection closed the stream without response"))?;
        let resp = resp.context("failed to receive gRPC server reflection response")?;
        match resp.message_response {
            Some(
                tonic_reflection::pb::v1::server_reflection_response::MessageResponse::ErrorResponse(
                    err,
                ),
            ) => Err(reflection_error(err.error_code, &err.error_message)),
            Some(
                tonic_reflection::pb::v1::server_reflection_response::MessageResponse::FileDescriptorResponse(
                    r,
                ),
            ) => Ok(Reply::Files(r.file_descriptor_proto)),
            Some(
                tonic_reflection::pb::v1::server_reflection_response::MessageResponse::ListServicesResponse(
                    r,
                ),
            ) => Ok(Reply::Services(r.service.into_iter().map(|s| s.name).collect())),
            Some(_) => bail!("unexpected gRPC server reflection response type"),
            None => bail!("gRPC server reflection returned an empty response"),
        }
    }

    async fn call_v1alpha(&self, request: V1MessageRequest) -> Result<Reply> {
        let message_request = match request {
            V1MessageRequest::FileByFilename(s) => V1AlphaMessageRequest::FileByFilename(s),
            V1MessageRequest::FileContainingSymbol(s) => {
                V1AlphaMessageRequest::FileContainingSymbol(s)
            }
            V1MessageRequest::ListServices(s) => V1AlphaMessageRequest::ListServices(s),
            _ => bail!("unsupported gRPC server reflection request type"),
        };
        let req = V1AlphaServerReflectionRequest {
            host: String::new(),
            message_request: Some(message_request),
        };
        let stream = futures::stream::iter(vec![req]);
        let mut request = tonic::Request::new(stream);
        *request.metadata_mut() = self.metadata.clone();

        let mut client = V1AlphaClient::new(self.channel.clone());
        let mut stream = client
            .server_reflection_info(request)
            .await
            .map(|r| r.into_inner())
            .context("failed to call gRPC server reflection")?;
        let resp = stream
            .next()
            .await
            .ok_or_else(|| anyhow!("gRPC server reflection closed the stream without response"))?;
        let resp = resp.context("failed to receive gRPC server reflection response")?;
        match resp.message_response {
            Some(
                tonic_reflection::pb::v1alpha::server_reflection_response::MessageResponse::ErrorResponse(
                    err,
                ),
            ) => Err(reflection_error(err.error_code, &err.error_message)),
            Some(
                tonic_reflection::pb::v1alpha::server_reflection_response::MessageResponse::FileDescriptorResponse(
                    r,
                ),
            ) => Ok(Reply::Files(r.file_descriptor_proto)),
            Some(
                tonic_reflection::pb::v1alpha::server_reflection_response::MessageResponse::ListServicesResponse(
                    r,
                ),
            ) => Ok(Reply::Services(r.service.into_iter().map(|s| s.name).collect())),
            Some(_) => bail!("unexpected gRPC server reflection response type"),
            None => bail!("gRPC server reflection returned an empty response"),
        }
    }
}

enum Reply {
    Services(Vec<String>),
    Files(Vec<Vec<u8>>),
}

fn decode_files(bytes: Vec<Vec<u8>>) -> Result<Vec<FileDescriptorProto>> {
    bytes
        .iter()
        .map(|b| {
            FileDescriptorProto::decode(b.as_slice())
                .context("failed to decode file descriptor proto")
        })
        .collect()
}

fn reflection_error(error_code: i32, error_message: &str) -> anyhow::Error {
    if error_code == tonic::Code::NotFound as i32 {
        return anyhow!(IdlError::UnknownSymbol);
    }
    anyhow!(
        "gRPC server reflection error: {} (code = {})",
        error_message,
        error_code
    )
}

fn is_unimplemented(e: &anyhow::Error) -> bool {
    e.downcast_ref::<tonic::Status>()
        .is_some_and(|s| s.code() == tonic::Code::Unimplemented)
}

fn is_reflection_service(name: &str) -> bool {
    name == REFLECTION_SERVICE_V1 || name == REFLECTION_SERVICE_V1ALPHA
}
