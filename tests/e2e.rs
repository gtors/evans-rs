use futures::StreamExt;
use std::time::Duration;
use tonic::transport::{Channel, Server};
use tonic_reflection::server::Builder as ReflectionBuilder;

pub mod proto {
    tonic::include_proto!("test");
}

#[path = "e2e/server.rs"]
pub mod server;

pub struct TestServer {
    pub addr: std::net::SocketAddr,
    shutdown: Option<tokio::sync::oneshot::Sender<()>>,
    handle: tokio::task::JoinHandle<()>,
}

pub async fn spawn() -> TestServer {
    spawn_with_tls(false).await
}

pub async fn spawn_with_tls(tls: bool) -> TestServer {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();

    let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel::<()>();
    let fds = protox::compile(["testdata/proto/test.proto"], ["testdata/proto"]).unwrap();

    let reflection_v1 = ReflectionBuilder::configure()
        .register_file_descriptor_set(fds.clone())
        .build_v1()
        .unwrap();
    let reflection_v1alpha = ReflectionBuilder::configure()
        .register_file_descriptor_set(fds)
        .build_v1alpha()
        .unwrap();

    let handle = tokio::spawn(async move {
        let mut builder = Server::builder();
        if tls {
            let cert = std::fs::read("testdata/tls/server.crt").unwrap();
            let key = std::fs::read("testdata/tls/server.key").unwrap();
            let identity = tonic::transport::Identity::from_pem(cert, key);
            builder = builder
                .tls_config(tonic::transport::ServerTlsConfig::new().identity(identity))
                .unwrap();
        }
        builder
            .add_service(proto::api_service_server::ApiServiceServer::new(
                server::TestApiService,
            ))
            .add_service(reflection_v1)
            .add_service(reflection_v1alpha)
            .serve_with_incoming(tokio_stream::wrappers::TcpListenerStream::new(listener))
            .await
            .unwrap();
        let _ = shutdown_rx.await;
    });

    wait_ready(addr).await;

    TestServer {
        addr,
        shutdown: Some(shutdown_tx),
        handle,
    }
}

async fn wait_ready(addr: std::net::SocketAddr) {
    for _ in 0..50 {
        if tokio::net::TcpStream::connect(addr).await.is_ok() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    panic!("test server did not become ready");
}

impl TestServer {
    pub async fn channel(&self) -> Channel {
        tonic::transport::Endpoint::from_shared(format!("http://{}", self.addr))
            .unwrap()
            .connect()
            .await
            .unwrap()
    }
}

impl Drop for TestServer {
    fn drop(&mut self) {
        if let Some(shutdown) = self.shutdown.take() {
            let _ = shutdown.send(());
        }
        self.handle.abort();
    }
}

#[tokio::test]
async fn reflection_lists_services_and_loads_descriptors() {
    let ts = spawn().await;
    let pool = evans2::idl::reflection::load(ts.channel().await, &Default::default())
        .await
        .unwrap();
    let idl = evans2::idl::Idl::from_pool(pool);
    assert_eq!(idl.list_packages(), vec!["test"]);
    assert_eq!(
        idl.list_services(),
        vec!["test.APIService", "test.EmptyService"]
    );

    let unary = idl.method("test.APIService", "Unary").unwrap();
    assert_eq!(unary.input().full_name(), "test.UnaryRequest");
    let bidi = idl
        .method("test.APIService", "BidirectionalStream")
        .unwrap();
    assert!(bidi.is_client_streaming() && bidi.is_server_streaming());
}

#[tokio::test]
async fn reflection_falls_back_to_v1alpha() {
    let ts = spawn().await;
    let channel = ts.channel().await;
    let mut client =
        tonic_reflection::pb::v1alpha::server_reflection_client::ServerReflectionClient::new(
            channel,
        );
    let req = tonic::Request::new(futures::stream::iter(vec![
        tonic_reflection::pb::v1alpha::ServerReflectionRequest {
            host: String::new(),
            message_request: Some(
                tonic_reflection::pb::v1alpha::server_reflection_request::MessageRequest::ListServices(
                    String::new(),
                ),
            ),
        },
    ]));
    let mut stream = client
        .server_reflection_info(req)
        .await
        .unwrap()
        .into_inner();
    let resp = stream.next().await.unwrap().unwrap();
    assert!(resp.message_response.is_some());
}

use prost::Name as _;

#[test]
fn generated_messages_exist() {
    assert_eq!(proto::UnaryRequest::NAME, "UnaryRequest");
    assert_eq!(proto::UnaryResponse::NAME, "UnaryResponse");
    assert_eq!(proto::UnaryRequest::full_name(), "test.UnaryRequest");
}

mod invoke {
    use prost_reflect::{DynamicMessage, Value};

    use super::*;

    fn pool() -> prost_reflect::DescriptorPool {
        let fds = protox::compile(["testdata/proto/test.proto"], ["testdata/proto"]).unwrap();
        prost_reflect::DescriptorPool::from_file_descriptor_set(fds).unwrap()
    }

    async fn client(addr: std::net::SocketAddr, echo: &str) -> evans2::grpc::Client {
        let server = evans2::config::ServerConfig {
            host: addr.ip().to_string(),
            port: addr.port().to_string(),
            ..Default::default()
        };
        let mut request = evans2::config::RequestConfig::default();
        request
            .header
            .insert("x-evans-echo".to_string(), vec![echo.to_string()]);
        evans2::grpc::Client::new(&server, &request).await.unwrap()
    }

    fn request_message(
        idl: &evans2::idl::Idl,
        service: &str,
        method: &str,
        name: &str,
    ) -> prost_reflect::DynamicMessage {
        let m = idl.method(service, method).unwrap();
        let mut msg = DynamicMessage::new(m.input());
        msg.set_field_by_name("name", Value::String(name.to_string()));
        msg
    }

    fn field_str(msg: &DynamicMessage, name: &str) -> String {
        msg.get_field_by_name(name)
            .unwrap()
            .as_str()
            .unwrap()
            .to_string()
    }

    #[tokio::test]
    async fn unary_roundtrip_with_headers_and_trailers() {
        let ts = spawn().await;
        let idl = evans2::idl::Idl::from_pool(pool());
        let method = idl.method("test.APIService", "Unary").unwrap();
        let mut msg = request_message(&idl, "test.APIService", "Unary", "world");
        msg.set_field_by_name("age", Value::I32(3));
        msg.set_field_by_name("suit", Value::EnumNumber(1));

        let mut client = client(ts.addr, "hello").await;
        let result = client.invoke(&method, msg).await.unwrap();

        assert_eq!(
            field_str(result.message.as_ref().unwrap(), "message"),
            "world"
        );
        assert_eq!(
            result
                .message
                .as_ref()
                .unwrap()
                .get_field_by_name("age")
                .unwrap()
                .as_i32(),
            Some(3)
        );
        assert_eq!(
            result
                .message
                .as_ref()
                .unwrap()
                .get_field_by_name("suit")
                .unwrap()
                .as_enum_number(),
            Some(1)
        );
        assert_eq!(result.headers.get("x-evans-echo").unwrap(), "hello");
        assert_eq!(result.trailers.get("grpc-status").unwrap(), "0");
    }

    #[tokio::test]
    async fn server_streaming_yields_all_messages() {
        let ts = spawn().await;
        let idl = evans2::idl::Idl::from_pool(pool());
        let method = idl.method("test.APIService", "ServerStream").unwrap();
        let msg = request_message(&idl, "test.APIService", "ServerStream", "stream");

        let mut client = client(ts.addr, "hello").await;
        let result = client.invoke_server_streaming(&method, msg).await.unwrap();

        let mut messages = Vec::new();
        let mut stream = result.stream.unwrap();
        while let Some(msg) = stream.message().await.unwrap() {
            messages.push(field_str(&msg, "message"));
        }
        assert_eq!(messages, vec!["stream-0", "stream-1", "stream-2"]);
        assert_eq!(result.headers.get("x-evans-echo").unwrap(), "hello");
    }

    #[tokio::test]
    async fn client_streaming_joins_messages() {
        let ts = spawn().await;
        let idl = evans2::idl::Idl::from_pool(pool());
        let method = idl.method("test.APIService", "ClientStream").unwrap();

        let names = ["a", "b", "c"];
        let input = method.input();
        let requests = futures::stream::iter(names.into_iter().map(move |name| {
            let mut msg = DynamicMessage::new(input.clone());
            msg.set_field_by_name("name", Value::String(name.to_string()));
            msg
        }));

        let mut client = client(ts.addr, "hello").await;
        let result = client
            .invoke_client_streaming(&method, requests)
            .await
            .unwrap();
        assert_eq!(
            field_str(result.message.as_ref().unwrap(), "message"),
            "a,b,c"
        );
    }

    #[tokio::test]
    async fn bidirectional_streaming_echoes_each_message() {
        let ts = spawn().await;
        let idl = evans2::idl::Idl::from_pool(pool());
        let method = idl
            .method("test.APIService", "BidirectionalStream")
            .unwrap();
        let input = method.input();

        let mut client = client(ts.addr, "hello").await;
        let result = client.invoke_bidi_streaming(&method, 8).await.unwrap();
        for name in ["x", "y", "z"] {
            let mut msg = DynamicMessage::new(input.clone());
            msg.set_field_by_name("name", Value::String(name.to_string()));
            result.sender.send(msg).await.unwrap();
        }
        drop(result.sender);

        let mut stream = result.stream.unwrap();
        let mut received = Vec::new();
        while let Some(msg) = stream.message().await.unwrap() {
            received.push(field_str(&msg, "message"));
        }
        assert_eq!(received, vec!["x", "y", "z"]);
        assert_eq!(result.headers.get("x-evans-echo").unwrap(), "hello");
    }

    #[tokio::test]
    async fn unimplemented_rpc_reports_status() {
        let ts = spawn().await;
        let idl = evans2::idl::Idl::from_pool(pool());
        let method = idl.method("test.EmptyService", "Missing").unwrap();
        let msg = request_message(&idl, "test.EmptyService", "Missing", "world");

        let mut client = client(ts.addr, "hello").await;
        // A failed call reports its status instead of returning an error.
        let result = client.invoke(&method, msg).await.unwrap();
        assert_eq!(result.status.code(), tonic::Code::Unimplemented);
        assert!(result.message.is_none());
    }

    #[tokio::test]
    async fn invalid_request_header_is_rejected() {
        let ts = spawn().await;
        let server = evans2::config::ServerConfig {
            host: ts.addr.ip().to_string(),
            port: ts.addr.port().to_string(),
            ..Default::default()
        };
        let mut request = evans2::config::RequestConfig::default();
        request
            .header
            .insert("invalid header".to_string(), vec!["v".to_string()]);
        let err = evans2::grpc::Client::new(&server, &request)
            .await
            .unwrap_err();
        assert!(err.to_string().contains("invalid header name"));
    }

    #[tokio::test]
    async fn tls_unary_roundtrip() {
        let ts = spawn_with_tls(true).await;
        let idl = evans2::idl::Idl::from_pool(pool());
        let method = idl.method("test.APIService", "Unary").unwrap();
        let msg = request_message(&idl, "test.APIService", "Unary", "secure");

        let server = evans2::config::ServerConfig {
            host: ts.addr.ip().to_string(),
            port: ts.addr.port().to_string(),
            tls: true,
            ..Default::default()
        };
        let request = evans2::config::RequestConfig {
            ca_cert_file: "testdata/tls/ca.crt".to_string(),
            ..Default::default()
        };

        let mut client = evans2::grpc::Client::new(&server, &request).await.unwrap();
        let result = client.invoke(&method, msg).await.unwrap();
        assert_eq!(
            field_str(result.message.as_ref().unwrap(), "message"),
            "secure"
        );
    }

    #[tokio::test]
    async fn tls_untrusted_certificate_is_rejected() {
        let ts = spawn_with_tls(true).await;
        let idl = evans2::idl::Idl::from_pool(pool());
        let method = idl.method("test.APIService", "Unary").unwrap();
        let msg = request_message(&idl, "test.APIService", "Unary", "secure");

        let server = evans2::config::ServerConfig {
            host: ts.addr.ip().to_string(),
            port: ts.addr.port().to_string(),
            tls: true,
            ..Default::default()
        };

        // The client connects lazily, so the certificate error appears at the call.
        let mut client =
            evans2::grpc::Client::new(&server, &evans2::config::RequestConfig::default())
                .await
                .unwrap();
        let err = client.invoke(&method, msg).await.unwrap_err();
        assert!(
            format!("{err:#}").contains("invalid peer certificate"),
            "unexpected error: {err:#}"
        );
    }
}

mod cli {
    use tokio::io::AsyncWriteExt;
    use tokio::process::Command;

    use super::*;

    /// Runs the evans binary with the given arguments and stdin.
    async fn run(args: &[&str], stdin: &str) -> (String, String, i32) {
        let mut child = Command::new(env!("CARGO_BIN_EXE_evans"))
            .args(args)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .expect("failed to spawn evans");

        let mut child_stdin = child.stdin.take().expect("stdin is piped");
        child_stdin
            .write_all(stdin.as_bytes())
            .await
            .expect("failed to write to stdin");
        drop(child_stdin);

        let output = child
            .wait_with_output()
            .await
            .expect("failed to wait for evans");
        (
            String::from_utf8_lossy(&output.stdout).into_owned(),
            String::from_utf8_lossy(&output.stderr).into_owned(),
            output.status.code().unwrap_or(-1),
        )
    }

    #[tokio::test]
    async fn calls_a_unary_rpc_from_stdin() {
        let ts = spawn().await;
        let (stdout, stderr, code) = run(
            &[
                "--proto",
                "testdata/proto/test.proto",
                "--port",
                &ts.addr.port().to_string(),
                "cli",
                "call",
                "test.APIService.Unary",
            ],
            r#"{"name": "ktr"}"#,
        )
        .await;

        assert_eq!(code, 0, "stderr: {stderr}");
        assert!(stdout.contains(r#""message": "ktr""#), "{stdout}");
    }

    #[tokio::test]
    async fn reports_a_failed_unary_rpc() {
        let ts = spawn().await;
        let (stdout, stderr, code) = run(
            &[
                "--proto",
                "testdata/proto/test.proto",
                "--port",
                &ts.addr.port().to_string(),
                "cli",
                "call",
                "test.APIService.UnaryFailure",
            ],
            r#"{"name": "ktr"}"#,
        )
        .await;

        // A failed call is reported with a non-zero exit code.
        assert_eq!(code, 1, "stderr: {stderr}");
        assert!(
            stderr.contains("rpc error: code = Internal desc = internal error"),
            "stderr: {stderr}"
        );
        // Without --enrich, stdout stays empty.
        assert_eq!(stdout, "", "stdout: {stdout}");
    }

    #[tokio::test]
    async fn reports_a_failed_unary_rpc_with_the_enrich_flag() {
        let ts = spawn().await;
        let (stdout, stderr, code) = run(
            &[
                "--proto",
                "testdata/proto/test.proto",
                "--port",
                &ts.addr.port().to_string(),
                "cli",
                "call",
                "--enrich",
                "test.APIService.UnaryFailure",
            ],
            r#"{"name": "ktr"}"#,
        )
        .await;

        assert_eq!(code, 1, "stderr: {stderr}");
        // The response of the failed call is written to stdout.
        assert!(stdout.contains("code: Internal"), "stdout: {stdout}");
        assert!(stdout.contains("number: 13"), "stdout: {stdout}");
        assert!(
            stdout.contains(r#"message: "internal error""#),
            "stdout: {stdout}"
        );
        // The message of the response is not written.
        assert!(!stdout.contains("\"message\":"), "stdout: {stdout}");
    }

    #[tokio::test]
    async fn reports_a_failed_unary_rpc_with_the_json_output_format() {
        let ts = spawn().await;
        let (stdout, stderr, code) = run(
            &[
                "--proto",
                "testdata/proto/test.proto",
                "--port",
                &ts.addr.port().to_string(),
                "cli",
                "call",
                "--enrich",
                "--output",
                "json",
                "test.APIService.UnaryFailure",
            ],
            r#"{"name": "ktr"}"#,
        )
        .await;

        assert_eq!(code, 1, "stderr: {stderr}");
        let value: serde_json::Value = serde_json::from_str(&stdout).expect("stdout is JSON");
        assert_eq!(value["status"]["code"], "Internal");
        assert_eq!(value["status"]["number"], 13);
        assert_eq!(value["status"]["message"], "internal error");
        // The details of the status are rendered like the original does.
        assert_eq!(
            value["status"]["details"][0]["@type"],
            "type.googleapis.com/google.rpc.BadRequest"
        );
        assert_eq!(
            value["status"]["details"][0]["fieldViolations"][0]["field"],
            "field"
        );
        // The messages of the failed call are omitted.
        assert!(value.get("messages").is_none(), "stdout: {stdout}");
    }

    #[tokio::test]
    async fn calls_a_unary_rpc_with_the_json_output_format() {
        let ts = spawn().await;
        let (stdout, stderr, code) = run(
            &[
                "--proto",
                "testdata/proto/test.proto",
                "--port",
                &ts.addr.port().to_string(),
                "cli",
                "call",
                "--output",
                "json",
                "test.APIService.Unary",
            ],
            r#"{"name": "ktr"}"#,
        )
        .await;

        assert_eq!(code, 0, "stderr: {stderr}");
        let value: serde_json::Value = serde_json::from_str(&stdout).expect("stdout is JSON");
        assert_eq!(value["messages"][0]["message"], "ktr");
        assert_eq!(value["status"]["code"], "OK");
    }

    #[tokio::test]
    async fn calls_a_unary_rpc_from_a_file() {
        let ts = spawn().await;
        let path = std::env::temp_dir().join(format!("evans-e2e-{}.json", std::process::id()));
        std::fs::write(&path, r#"{"name": "from-file"}"#).unwrap();

        let (stdout, stderr, code) = run(
            &[
                "--proto",
                "testdata/proto/test.proto",
                "--port",
                &ts.addr.port().to_string(),
                "cli",
                "call",
                "--file",
                path.to_str().unwrap(),
                "test.APIService.Unary",
            ],
            "",
        )
        .await;

        assert_eq!(code, 0, "stderr: {stderr}");
        assert!(stdout.contains(r#""message": "from-file""#), "{stdout}");
        std::fs::remove_file(&path).ok();
    }

    #[tokio::test]
    async fn calls_a_server_streaming_rpc() {
        let ts = spawn().await;
        let (stdout, stderr, code) = run(
            &[
                "--proto",
                "testdata/proto/test.proto",
                "--port",
                &ts.addr.port().to_string(),
                "cli",
                "call",
                "test.APIService.ServerStream",
            ],
            r#"{"name": "ktr"}"#,
        )
        .await;

        assert_eq!(code, 0, "stderr: {stderr}");
        assert!(stdout.contains(r#""message": "ktr-0""#), "{stdout}");
        assert!(stdout.contains(r#""message": "ktr-1""#), "{stdout}");
        assert!(stdout.contains(r#""message": "ktr-2""#), "{stdout}");
    }

    #[tokio::test]
    async fn calls_a_client_streaming_rpc() {
        let ts = spawn().await;
        let (stdout, stderr, code) = run(
            &[
                "--proto",
                "testdata/proto/test.proto",
                "--port",
                &ts.addr.port().to_string(),
                "cli",
                "call",
                "test.APIService.ClientStream",
            ],
            "{\"name\": \"a\"}\n{\"name\": \"b\"}\n",
        )
        .await;

        assert_eq!(code, 0, "stderr: {stderr}");
        assert!(stdout.contains(r#""message": "a,b""#), "{stdout}");
    }

    #[tokio::test]
    async fn reports_an_error_for_an_unknown_method() {
        let ts = spawn().await;
        let (stdout, stderr, code) = run(
            &[
                "--proto",
                "testdata/proto/test.proto",
                "--port",
                &ts.addr.port().to_string(),
                "cli",
                "call",
                "test.APIService.Nope",
            ],
            "{}",
        )
        .await;

        assert_eq!(code, 1);
        assert!(stdout.is_empty(), "{stdout}");
        assert!(
            stderr.contains("failed to get the RPC descriptor"),
            "{stderr}"
        );
    }

    #[tokio::test]
    async fn reports_an_error_for_an_invalid_request_body() {
        let ts = spawn().await;
        let (stdout, stderr, code) = run(
            &[
                "--proto",
                "testdata/proto/test.proto",
                "--port",
                &ts.addr.port().to_string(),
                "cli",
                "call",
                "test.APIService.Unary",
            ],
            r#"{"nope": true}"#,
        )
        .await;

        assert_eq!(code, 1);
        assert!(stdout.is_empty(), "{stdout}");
        assert!(
            stderr.contains("unrecognized field name 'nope'"),
            "{stderr}"
        );
    }

    #[tokio::test]
    async fn calls_a_unary_rpc_with_the_enriched_output() {
        let ts = spawn().await;
        let (stdout, stderr, code) = run(
            &[
                "--proto",
                "testdata/proto/test.proto",
                "--port",
                &ts.addr.port().to_string(),
                "cli",
                "call",
                "--enrich",
                "test.APIService.Unary",
            ],
            r#"{"name": "ktr"}"#,
        )
        .await;

        assert_eq!(code, 0, "stderr: {stderr}");
        assert!(
            stdout.contains("content-type: application/grpc"),
            "{stdout}"
        );
        assert!(stdout.contains(r#""message": "ktr""#), "{stdout}");
        assert!(stdout.contains("code: OK"), "{stdout}");
    }

    #[tokio::test]
    async fn calls_a_unary_rpc_with_the_default_values() {
        let ts = spawn().await;
        let (stdout, stderr, code) = run(
            &[
                "--proto",
                "testdata/proto/test.proto",
                "--port",
                &ts.addr.port().to_string(),
                "cli",
                "call",
                "--emit-defaults",
                "test.APIService.Unary",
            ],
            r#"{"name": "ktr"}"#,
        )
        .await;

        assert_eq!(code, 0, "stderr: {stderr}");
        assert!(stdout.contains(r#""suit": "SUIT_UNSPECIFIED""#), "{stdout}");
    }

    #[tokio::test]
    async fn lists_services_without_a_server() {
        let (stdout, stderr, code) =
            run(&["--proto", "testdata/proto/test.proto", "cli", "list"], "").await;

        assert_eq!(code, 0, "stderr: {stderr}");
        assert_eq!(stdout, "test.APIService\ntest.EmptyService\n");
    }

    #[tokio::test]
    async fn lists_methods_with_the_json_format() {
        let (stdout, stderr, code) = run(
            &[
                "--proto",
                "testdata/proto/test.proto",
                "cli",
                "list",
                "-o",
                "json",
                "test.APIService",
            ],
            "",
        )
        .await;

        assert_eq!(code, 0, "stderr: {stderr}");
        let value: serde_json::Value = serde_json::from_str(&stdout).expect("stdout is JSON");
        let methods = value["methods"].as_array().expect("methods is an array");
        assert_eq!(
            methods[0]["fully_qualified_name"],
            "test.APIService.BidirectionalStream"
        );
        assert_eq!(methods[0]["request_type"], "test.UnaryRequest");
    }

    #[tokio::test]
    async fn describes_a_symbol() {
        let (stdout, stderr, code) = run(
            &[
                "--proto",
                "testdata/proto/test.proto",
                "cli",
                "desc",
                "test.APIService.Unary",
            ],
            "",
        )
        .await;

        assert_eq!(code, 0, "stderr: {stderr}");
        assert_eq!(
            stdout,
            "test.APIService.Unary:\nrpc Unary ( .test.UnaryRequest ) returns ( .test.UnaryResponse );\n"
        );
    }

    #[tokio::test]
    async fn reports_an_error_for_an_unknown_symbol() {
        let (stdout, stderr, code) = run(
            &[
                "--proto",
                "testdata/proto/test.proto",
                "cli",
                "desc",
                "test.Nope",
            ],
            "",
        )
        .await;

        assert_eq!(code, 1);
        assert!(stdout.is_empty(), "{stdout}");
        assert!(
            stderr.contains("failed to resolve symbol 'test.Nope'"),
            "{stderr}"
        );
    }
}

mod message {
    use evans2::message::{self, JsonOptions};
    use prost_reflect::{DescriptorPool, DynamicMessage, Value};

    use super::*;

    fn pool() -> DescriptorPool {
        let fds = protox::compile(["testdata/proto/test.proto"], ["testdata/proto"]).unwrap();
        DescriptorPool::from_file_descriptor_set(fds).unwrap()
    }

    fn request_desc() -> prost_reflect::MessageDescriptor {
        pool().get_message_by_name("test.UnaryRequest").unwrap()
    }

    #[tokio::test]
    async fn json_request_is_sent_and_response_is_rendered() {
        let ts = spawn().await;
        let idl = evans2::idl::Idl::from_pool(pool());
        let method = idl.method("test.APIService", "Unary").unwrap();

        let msg = message::from_json(
            &request_desc(),
            r#"{
                "name": "ktr",
                "tags": ["a", "b"],
                "suit": "DIAMONDS",
                "age": 20,
                "text": "hello"
            }"#,
        )
        .unwrap();

        let server = evans2::config::ServerConfig {
            host: ts.addr.ip().to_string(),
            port: ts.addr.port().to_string(),
            ..Default::default()
        };
        let mut client =
            evans2::grpc::Client::new(&server, &evans2::config::RequestConfig::default())
                .await
                .unwrap();

        let result = client.invoke(&method, msg).await.unwrap();
        let json =
            message::to_json(result.message.as_ref().unwrap(), JsonOptions::default()).unwrap();

        assert!(json.contains(r#""message":"ktr""#), "{json}");
        assert!(json.contains(r#""suit":"DIAMONDS""#), "{json}");
        assert!(json.contains(r#""age":20"#), "{json}");
        assert!(json.contains(r#""tags":["a","b"]"#), "{json}");
        assert!(json.contains(r#""text":"hello""#), "{json}");
    }

    #[tokio::test]
    async fn json_request_rejects_unknown_fields() {
        let err = message::from_json(&request_desc(), r#"{"nope": 1}"#).unwrap_err();
        assert!(
            format!("{err:#}").contains("unrecognized field name 'nope'"),
            "unexpected error: {err:#}"
        );
    }

    #[tokio::test]
    async fn json_request_supports_proto_field_names() {
        let msg = message::from_json(&request_desc(), r#"{"name": "ktr"}"#).unwrap();
        assert_eq!(msg.get_field_by_name("name").unwrap().as_str(), Some("ktr"));
    }

    #[tokio::test]
    async fn optional_fields_keep_their_presence() {
        let msg = message::from_json(&request_desc(), r#"{"age": 0}"#).unwrap();
        assert!(msg.has_field_by_name("age"));

        let msg = message::from_json(&request_desc(), "{}").unwrap();
        assert!(!msg.has_field_by_name("age"));
    }

    #[tokio::test]
    async fn oneof_accepts_only_one_member() {
        let msg = message::from_json(&request_desc(), r#"{"text": "hello"}"#).unwrap();
        assert!(msg.has_field_by_name("text"));
        assert!(!msg.has_field_by_name("payload"));

        let err =
            message::from_json(&request_desc(), r#"{"text": "a", "payload": "Yg=="}"#).unwrap_err();
        assert!(
            format!("{err:#}").contains("multiple fields provided for oneof"),
            "unexpected error: {err:#}"
        );
    }

    #[test]
    fn binary_roundtrip_keeps_all_field_kinds() {
        let mut msg = DynamicMessage::new(request_desc());
        msg.set_field_by_name("name", Value::String("ktr".to_string()));
        msg.set_field_by_name("suit", Value::EnumNumber(3));
        msg.set_field_by_name(
            "tags",
            Value::List(vec![
                Value::String("a".to_string()),
                Value::String("b".to_string()),
            ]),
        );

        let bytes = message::to_binary(&msg).unwrap();
        let decoded = message::from_binary(&request_desc(), &bytes).unwrap();

        assert_eq!(
            decoded.get_field_by_name("name").unwrap().as_str(),
            Some("ktr")
        );
        assert_eq!(
            decoded.get_field_by_name("suit").unwrap().as_enum_number(),
            Some(3)
        );
        assert_eq!(
            decoded
                .get_field_by_name("tags")
                .unwrap()
                .as_list()
                .unwrap()
                .len(),
            2
        );
    }
}

mod repl {
    use std::io;
    use std::sync::{Arc, Mutex};

    use evans2::message::filler::{FillError, Prompter};
    use evans2::repl::session::Session;

    use super::*;

    /// A session that replays scripted command lines and answers.
    struct ScriptedSession {
        lines: Vec<String>,
        answers: Vec<String>,
        selections: Vec<usize>,
    }

    impl ScriptedSession {
        fn new(lines: &[&str], answers: &[&str], selections: &[usize]) -> Self {
            Self {
                lines: lines.iter().map(|s| s.to_string()).collect(),
                answers: answers.iter().map(|s| s.to_string()).collect(),
                selections: selections.to_vec(),
            }
        }
    }

    impl Session for ScriptedSession {
        fn read_line(&mut self, _prompt: &str) -> io::Result<Option<String>> {
            if self.lines.is_empty() {
                return Ok(None);
            }
            Ok(Some(self.lines.remove(0)))
        }

        fn prompter(&mut self) -> Box<dyn Prompter> {
            Box::new(ScriptedPrompter {
                answers: std::mem::take(&mut self.answers),
                selections: std::mem::take(&mut self.selections),
            })
        }
    }

    struct ScriptedPrompter {
        answers: Vec<String>,
        selections: Vec<usize>,
    }

    impl Prompter for ScriptedPrompter {
        fn input(&mut self, _prefix: &str) -> Result<String, FillError> {
            if self.answers.is_empty() {
                return Err(FillError::Eof);
            }
            Ok(self.answers.remove(0))
        }

        fn select(&mut self, _message: &str, _options: &[String]) -> Result<usize, FillError> {
            if self.selections.is_empty() {
                return Err(FillError::Abort);
            }
            Ok(self.selections.remove(0))
        }
    }

    /// The output of a REPL session.
    #[derive(Debug)]
    struct ReplOutput {
        out: String,
        err: String,
    }

    async fn run_repl(
        ts: &TestServer,
        lines: &[&str],
        answers: &[&str],
        selections: &[usize],
    ) -> anyhow::Result<ReplOutput> {
        run_repl_with_headers(ts, lines, answers, selections, &[]).await
    }

    async fn run_repl_with_headers(
        ts: &TestServer,
        lines: &[&str],
        answers: &[&str],
        selections: &[usize],
        headers: &[(&str, &str)],
    ) -> anyhow::Result<ReplOutput> {
        let mut cfg = evans2::config::Config {
            default: evans2::config::DefaultConfig {
                proto_path: vec![".".to_string()],
                proto_file: vec!["testdata/proto/test.proto".to_string()],
                ..Default::default()
            },
            server: evans2::config::ServerConfig {
                host: ts.addr.ip().to_string(),
                port: ts.addr.port().to_string(),
                ..Default::default()
            },
            ..Default::default()
        };
        for (key, value) in headers {
            cfg.request
                .header
                .insert(key.to_string(), vec![value.to_string()]);
        }

        let pool = {
            let fds = protox::compile(["testdata/proto/test.proto"], ["testdata/proto"]).unwrap();
            prost_reflect::DescriptorPool::from_file_descriptor_set(fds).unwrap()
        };
        let idl = evans2::idl::Idl::from_pool(pool);

        let mut client = evans2::grpc::Client::new(&cfg.server, &cfg.request)
            .await
            .unwrap();

        let session = ScriptedSession::new(lines, answers, selections);
        let out: Arc<Mutex<Vec<u8>>> = Arc::new(Mutex::new(Vec::new()));
        let err: Arc<Mutex<Vec<u8>>> = Arc::new(Mutex::new(Vec::new()));
        let mut repl = evans2::repl::Repl::with_writers(
            &cfg,
            &idl,
            &mut client,
            Box::new(session),
            Box::new(SharedWriter(Arc::clone(&out))),
            Box::new(SharedWriter(Arc::clone(&err))),
        )
        .unwrap();

        repl.run().await?;

        let out = String::from_utf8_lossy(&out.lock().unwrap().clone()).into_owned();
        let err = String::from_utf8_lossy(&err.lock().unwrap().clone()).into_owned();
        Ok(ReplOutput { out, err })
    }

    struct SharedWriter(Arc<Mutex<Vec<u8>>>);

    impl std::io::Write for SharedWriter {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(buf);
            Ok(buf.len())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    #[tokio::test]
    async fn calls_a_unary_rpc() {
        let ts = spawn().await;
        // The request has the name, the repeated tags, the suit, the optional age, the
        // oneof body and the nested message. The empty answers use the default values.
        let out = run_repl(
            &ts,
            &["package test", "service APIService", "call Simple"],
            &["ktr"],
            &[],
        )
        .await
        .unwrap();
        assert!(out.out.contains(r#""message": "ktr""#), "{out:?}");
    }

    #[tokio::test]
    async fn calls_a_unary_rpc_with_a_fully_qualified_service() {
        let ts = spawn().await;
        run_repl(
            &ts,
            &["service test.APIService", "call Unary"],
            &["ktr"],
            &[0],
        )
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn shows_packages_services_and_methods() {
        let ts = spawn().await;
        let out = run_repl(
            &ts,
            &["show package", "show service", "show rpc", "show message"],
            &[],
            &[],
        )
        .await
        .unwrap();
        assert!(out.out.contains("| test  "), "{out:?}");
        assert!(out.out.contains("test.APIService"), "{out:?}");
        assert!(out.out.contains("test.APIService.Unary"), "{out:?}");
        assert!(out.out.contains("UnaryRequest"), "{out:?}");
    }

    #[tokio::test]
    async fn describes_a_symbol() {
        let ts = spawn().await;
        run_repl(&ts, &["desc test.UnaryRequest"], &[], &[])
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn reports_an_unknown_command() {
        let ts = spawn().await;
        let out = run_repl(&ts, &["nope"], &[], &[]).await.unwrap();
        assert!(out.err.contains("unknown command 'nope'"), "{out:?}");
    }

    #[tokio::test]
    async fn reports_an_unknown_package() {
        let ts = spawn().await;
        let out = run_repl(&ts, &["package nope"], &[], &[]).await.unwrap();
        assert!(out.err.contains("unknown package name 'nope'"), "{out:?}");
    }

    #[tokio::test]
    async fn exits_with_the_exit_command() {
        let ts = spawn().await;
        run_repl(&ts, &["exit", "call Unary"], &["ktr"], &[0])
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn calls_a_server_streaming_rpc() {
        let ts = spawn().await;
        let out = run_repl(
            &ts,
            &["package test", "service APIService", "call SimpleStream"],
            &["ktr"],
            &[],
        )
        .await
        .unwrap();
        assert!(out.out.contains(r#""message": "ktr-0""#), "{out:?}");
        assert!(out.out.contains(r#""message": "ktr-2""#), "{out:?}");
    }

    #[tokio::test]
    async fn calls_a_client_streaming_rpc() {
        let ts = spawn().await;
        let out = run_repl(
            &ts,
            &[
                "package test",
                "service APIService",
                "call SimpleClientStream",
            ],
            &["a", "b"],
            &[],
        )
        .await
        .unwrap();
        assert!(out.out.contains(r#""message": "a,b""#), "{out:?}");
    }

    #[tokio::test]
    async fn repeats_the_previous_request() {
        let ts = spawn().await;
        let out = run_repl(
            &ts,
            &[
                "package test",
                "service APIService",
                "call Simple",
                "call --repeat Simple",
            ],
            &["ktr"],
            &[],
        )
        .await
        .unwrap();
        assert_eq!(out.out.matches(r#""message": "ktr""#).count(), 2, "{out:?}");
    }

    #[tokio::test]
    async fn reports_an_error_when_repeating_an_unknown_request() {
        let ts = spawn().await;
        let out = run_repl(
            &ts,
            &["package test", "service APIService", "call --repeat Simple"],
            &[],
            &[],
        )
        .await
        .unwrap();
        assert!(
            out.err.contains("no previous request exists for method"),
            "{out:?}"
        );
    }

    #[tokio::test]
    async fn sends_the_configured_headers_only_once() {
        let ts = spawn().await;
        let out = run_repl_with_headers(
            &ts,
            &["package test", "service APIService", "call Simple --enrich"],
            &["ktr"],
            &[],
            &[("x-evans-echo", "hi")],
        )
        .await
        .unwrap();
        assert!(out.out.contains("x-evans-echo-count: 1"), "{out:?}");
    }

    #[tokio::test]
    async fn repeats_the_previous_request_with_the_short_flag() {
        let ts = spawn().await;
        let out = run_repl(
            &ts,
            &[
                "package test",
                "service APIService",
                "call Simple",
                "call -r Simple",
            ],
            &["ktr"],
            &[],
        )
        .await
        .unwrap();
        assert_eq!(out.out.matches(r#""message": "ktr""#).count(), 2, "{out:?}");
    }

    #[tokio::test]
    async fn sets_headers() {
        let ts = spawn().await;
        let out = run_repl(&ts, &["header foo=bar", "show header"], &[], &[])
            .await
            .unwrap();
        assert!(out.out.contains("foo"), "{out:?}");
        assert!(out.out.contains("bar"), "{out:?}");
    }
}
