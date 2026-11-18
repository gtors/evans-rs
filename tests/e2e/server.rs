use futures::StreamExt;
use tonic::metadata::MetadataValue;
use tonic::{Request, Response, Status};

use super::proto;

pub struct TestApiService;

/// The details of the status of a failed call, like the ones of the original.
fn failure_details() -> Vec<u8> {
    use prost_reflect::{DynamicMessage, Value};

    let fds = protox::compile(
        [
            "proto/google/rpc/status.proto",
            "proto/google/rpc/error_details.proto",
        ],
        ["proto"],
    )
    .expect("failed to compile the google.rpc protos");
    let pool = prost_reflect::DescriptorPool::from_file_descriptor_set(fds)
        .expect("failed to build the descriptor pool");

    let status_desc = pool
        .get_message_by_name("google.rpc.Status")
        .expect("google.rpc.Status");
    let bad_request = pool
        .get_message_by_name("google.rpc.BadRequest")
        .expect("google.rpc.BadRequest");
    let violation = pool
        .get_message_by_name("google.rpc.BadRequest.FieldViolation")
        .expect("google.rpc.BadRequest.FieldViolation");
    let any_desc = pool
        .get_message_by_name("google.protobuf.Any")
        .expect("google.protobuf.Any");

    let mut violation_msg = DynamicMessage::new(violation.clone());
    violation_msg.set_field_by_name("field", Value::String("field".to_string()));
    violation_msg.set_field_by_name("description", Value::String("description".to_string()));

    let mut bad_request_msg = DynamicMessage::new(bad_request.clone());
    let violation_field = bad_request
        .get_field_by_name("field_violations")
        .expect("the field violations");
    bad_request_msg
        .get_field_mut(&violation_field)
        .as_list_mut()
        .expect("the field violations are a list")
        .push(Value::Message(violation_msg));

    let mut any = DynamicMessage::new(any_desc);
    any.set_field_by_name(
        "type_url",
        Value::String("type.googleapis.com/google.rpc.BadRequest".to_string()),
    );
    any.set_field_by_name(
        "value",
        Value::Bytes(prost_reflect::bytes::Bytes::from(
            prost_reflect::prost::Message::encode_to_vec(&bad_request_msg),
        )),
    );

    let details_field = status_desc
        .get_field_by_name("details")
        .expect("the details");
    let mut status = DynamicMessage::new(status_desc);
    status.set_field_by_name("code", Value::I32(13));
    status.set_field_by_name("message", Value::String("internal error".to_string()));
    status
        .get_field_mut(&details_field)
        .as_list_mut()
        .expect("the details are a list")
        .push(Value::Message(any));

    prost_reflect::prost::Message::encode_to_vec(&status)
}

type UnaryResult<T> = Result<Response<T>, Status>;
type StreamResult<T> =
    Result<Response<futures::stream::BoxStream<'static, Result<T, Status>>>, Status>;

fn echo_header(request: &Request<impl Sized>) -> Option<MetadataValue<tonic::metadata::Ascii>> {
    request.metadata().get("x-evans-echo").cloned()
}

/// The number of the values of the echoed header.
fn echo_header_count(request: &Request<impl Sized>) -> usize {
    request.metadata().get_all("x-evans-echo").iter().count()
}

#[tonic::async_trait]
impl proto::api_service_server::ApiService for TestApiService {
    async fn unary(
        &self,
        request: Request<proto::UnaryRequest>,
    ) -> UnaryResult<proto::UnaryResponse> {
        let echo = echo_header(&request);
        let echo_count = echo_header_count(&request);
        let req = request.into_inner();
        let text = match req.body {
            Some(proto::unary_request::Body::Text(text)) => text,
            _ => String::new(),
        };
        let mut resp = Response::new(proto::UnaryResponse {
            message: req.name,
            suit: req.suit,
            age: req.age,
            tags: req.tags,
            text,
        });
        if let Some(v) = echo {
            resp.metadata_mut().insert("x-evans-echo", v);
        }
        resp.metadata_mut().insert(
            "x-evans-echo-count",
            echo_count.to_string().parse().unwrap(),
        );
        Ok(resp)
    }

    async fn unary_failure(
        &self,
        request: Request<proto::UnaryRequest>,
    ) -> UnaryResult<proto::UnaryResponse> {
        let _ = request;
        Err(Status::with_details(
            tonic::Code::Internal,
            "internal error",
            failure_details().into(),
        ))
    }

    async fn simple(
        &self,
        request: Request<proto::SimpleRequest>,
    ) -> UnaryResult<proto::SimpleResponse> {
        let echo_count = echo_header_count(&request);
        let req = request.into_inner();
        let mut resp = Response::new(proto::SimpleResponse { message: req.name });
        resp.metadata_mut().insert(
            "x-evans-echo-count",
            echo_count.to_string().parse().unwrap(),
        );
        Ok(resp)
    }

    type SimpleStreamStream =
        futures::stream::BoxStream<'static, Result<proto::SimpleResponse, Status>>;

    async fn simple_stream(
        &self,
        request: Request<proto::SimpleRequest>,
    ) -> StreamResult<proto::SimpleResponse> {
        let name = request.into_inner().name;
        let msgs = (0..3).map(move |i| proto::SimpleResponse {
            message: format!("{name}-{i}"),
        });
        Ok(Response::new(futures::stream::iter(msgs).map(Ok).boxed()))
    }

    async fn simple_client_stream(
        &self,
        request: Request<tonic::Streaming<proto::SimpleRequest>>,
    ) -> UnaryResult<proto::SimpleResponse> {
        let mut stream = request.into_inner();
        let mut names = Vec::new();
        while let Some(req) = stream.next().await {
            names.push(req?.name);
        }
        Ok(Response::new(proto::SimpleResponse {
            message: names.join(","),
        }))
    }

    type ServerStreamStream =
        futures::stream::BoxStream<'static, Result<proto::UnaryResponse, Status>>;
    async fn server_stream(
        &self,
        request: Request<proto::UnaryRequest>,
    ) -> StreamResult<proto::UnaryResponse> {
        let echo = echo_header(&request);
        let req = request.into_inner();
        let name = req.name;
        let msgs = (0..3).map(move |i| proto::UnaryResponse {
            message: format!("{}-{}", name, i),
            suit: req.suit,
            age: req.age,
            tags: Vec::new(),
            text: String::new(),
        });
        let mut resp = Response::new(futures::stream::iter(msgs).map(Ok).boxed());
        if let Some(v) = echo {
            resp.metadata_mut().insert("x-evans-echo", v);
        }
        Ok(resp)
    }

    async fn client_stream(
        &self,
        request: Request<tonic::Streaming<proto::UnaryRequest>>,
    ) -> UnaryResult<proto::UnaryResponse> {
        let echo = echo_header(&request);
        let mut stream = request.into_inner();
        let mut names = Vec::new();
        while let Some(req) = stream.next().await {
            names.push(req?.name);
        }
        let mut resp = Response::new(proto::UnaryResponse {
            message: names.join(","),
            suit: proto::Suit::Unspecified as i32,
            age: None,
            tags: Vec::new(),
            text: String::new(),
        });
        if let Some(v) = echo {
            resp.metadata_mut().insert("x-evans-echo", v);
        }
        Ok(resp)
    }

    type BidirectionalStreamStream =
        futures::stream::BoxStream<'static, Result<proto::UnaryResponse, Status>>;

    async fn bidirectional_stream(
        &self,
        request: Request<tonic::Streaming<proto::UnaryRequest>>,
    ) -> StreamResult<proto::UnaryResponse> {
        let echo = echo_header(&request);
        let mut stream = request.into_inner();
        let (tx, rx) = tokio::sync::mpsc::channel(8);
        tokio::spawn(async move {
            while let Some(req) = stream.next().await {
                let Ok(req) = req else { break };
                let resp = proto::UnaryResponse {
                    message: req.name,
                    suit: req.suit,
                    age: req.age,
                    tags: req.tags,
                    text: String::new(),
                };
                if tx.send(Ok(resp)).await.is_err() {
                    break;
                }
            }
        });
        let mut resp = Response::new(tokio_stream::wrappers::ReceiverStream::new(rx).boxed());
        if let Some(v) = echo {
            resp.metadata_mut().insert("x-evans-echo", v);
        }
        Ok(resp)
    }
}
