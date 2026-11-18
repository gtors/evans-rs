//! The RPC call flow of the REPL, mirroring `usecase/call_rpc.go` of the original.

use anyhow::{Result, anyhow};
use prost_reflect::MethodDescriptor;
use tokio::sync::mpsc;
use tonic::Status;

use crate::grpc::{Client, Headers};
use crate::message::DynamicMessage;
use crate::message::filler::{FillError, Filler, Prompter};
use crate::present::response;

/// The options of a REPL call.
pub struct CallOptions<'a> {
    /// The headers that are sent with the request.
    pub headers: Headers,
    /// The previous request of the method, used by the `--repeat` option.
    pub previous: Option<&'a DynamicMessage>,
    /// Whether the previous request is repeated.
    pub repeat: bool,
    /// The renderer of the response.
    pub on_response: Option<&'a mut dyn ResponseRenderer>,
}

/// The parts of a response that are rendered as they arrive.
pub enum ResponseEvent<'a> {
    /// The headers of the response.
    Header(&'a tonic::metadata::MetadataMap),
    /// A message of the response.
    Message(&'a DynamicMessage),
}

/// Renders the parts of a response as they arrive.
pub trait ResponseRenderer {
    /// Renders a part of the response.
    fn render(&mut self, event: ResponseEvent<'_>) -> Result<()>;
}

/// The result of a REPL call.
pub struct CallResult {
    /// The response of the call.
    pub response: response::Response,
    /// The request that was sent, if it can be repeated.
    pub request: Option<DynamicMessage>,
}

/// Calls the method, filling the request interactively.
pub async fn repl_call<P: Prompter + ?Sized>(
    client: &mut Client,
    method: &MethodDescriptor,
    filler: &mut Filler<'_, P>,
    options: CallOptions<'_>,
) -> Result<CallResult> {
    let CallOptions {
        headers,
        previous,
        repeat,
        mut on_response,
    } = options;
    client.set_headers(headers);

    if repeat {
        let previous = previous.ok_or_else(|| {
            anyhow!(
                "no previous request exists for method: {}, please issue a normal request",
                method.full_name()
            )
        })?;
        if method.is_client_streaming() {
            return Err(anyhow!(
                "cannot rerun previous method: {} as client/bidi streaming RPCs are not supported",
                method.full_name()
            ));
        }
        let response = call_unary(client, method, previous.clone()).await?;
        if let Some(on_response) = on_response.as_mut() {
            on_response.render(ResponseEvent::Header(&response.headers))?;
            for message in &response.messages {
                on_response.render(ResponseEvent::Message(message))?;
            }
        }
        return Ok(CallResult {
            response,
            request: Some(previous.clone()),
        });
    }

    if method.is_client_streaming() && !method.is_server_streaming() {
        let response = call_client_streaming(client, method, filler).await?;
        if let Some(on_response) = on_response.as_mut() {
            on_response.render(ResponseEvent::Header(&response.headers))?;
            for message in &response.messages {
                on_response.render(ResponseEvent::Message(message))?;
            }
        }
        return Ok(CallResult {
            response,
            request: None,
        });
    }

    if method.is_server_streaming() && method.is_client_streaming() {
        let response = call_bidi_streaming(client, method, filler).await?;
        return Ok(CallResult {
            response,
            request: None,
        });
    }

    let request = fill(filler, method)?;

    if method.is_server_streaming() {
        let result = client
            .invoke_server_streaming(method, request.clone())
            .await?;
        let headers = result.headers.clone();
        if let Some(on_response) = on_response.as_mut() {
            on_response.render(ResponseEvent::Header(&headers))?;
        }

        // The messages are rendered as they arrive, like the original does.
        let (messages, trailers, status) = {
            let mut on_message = |message: &DynamicMessage| {
                if let Some(on_response) = on_response.as_mut() {
                    on_response.render(ResponseEvent::Message(message))?;
                }
                Ok(())
            };
            crate::grpc::collect_stream(result.stream, result.status, Some(&mut on_message)).await?
        };

        let response = response::Response {
            headers,
            messages,
            trailers,
            status,
        };
        return Ok(CallResult {
            response,
            request: Some(request),
        });
    }

    let response = call_unary(client, method, request.clone()).await?;
    if let Some(on_response) = on_response.as_mut() {
        on_response.render(ResponseEvent::Header(&response.headers))?;
        for message in &response.messages {
            on_response.render(ResponseEvent::Message(message))?;
        }
    }
    Ok(CallResult {
        response,
        request: Some(request),
    })
}

/// Calls a unary method with the given request.
async fn call_unary(
    client: &mut Client,
    method: &MethodDescriptor,
    request: DynamicMessage,
) -> Result<response::Response> {
    let result = client.invoke(method, request).await?;
    Ok(response::Response {
        headers: result.headers,
        messages: result.message.into_iter().collect(),
        trailers: result.trailers,
        status: result.status,
    })
}

/// Calls a client streaming method. The requests are filled until the user finishes the
/// input.
async fn call_client_streaming<P: Prompter + ?Sized>(
    client: &mut Client,
    method: &MethodDescriptor,
    filler: &mut Filler<'_, P>,
) -> Result<response::Response> {
    let mut requests = Vec::new();
    loop {
        match fill(filler, method) {
            Ok(msg) => requests.push(msg),
            Err(FillError::Eof) => break,
            Err(FillError::Abort) => break,
            Err(err) => return Err(anyhow!("{err}")),
        }
    }

    let result = client
        .invoke_client_streaming(method, futures::stream::iter(requests))
        .await?;
    Ok(response::Response {
        headers: result.headers,
        messages: result.message.into_iter().collect(),
        trailers: result.trailers,
        status: result.status,
    })
}

/// Calls a bidirectional streaming method. The requests are filled and sent one by one
/// while the responses are collected in the background.
async fn call_bidi_streaming<P: Prompter + ?Sized>(
    client: &mut Client,
    method: &MethodDescriptor,
    filler: &mut Filler<'_, P>,
) -> Result<response::Response> {
    let result = client.invoke_bidi_streaming(method, 8).await?;
    let sender = result.sender;

    // The responses are read by a background task so that they are not missed while the
    // user is inputting the next request.
    let (tx, mut rx) = mpsc::channel::<DynamicMessage>(64);
    let reader = result.stream.map(|mut stream| {
        tokio::spawn(async move {
            let mut status = Status::ok("");
            loop {
                match stream.message().await {
                    Ok(Some(message)) => {
                        if tx.send(message).await.is_err() {
                            break;
                        }
                    }
                    Ok(None) => break,
                    Err(err) => {
                        status = err;
                        break;
                    }
                }
            }
            let trailers = stream.trailers().await.ok().flatten().unwrap_or_default();
            (trailers, status)
        })
    });

    let mut messages = Vec::new();

    loop {
        let request = match fill(filler, method) {
            Ok(msg) => msg,
            Err(FillError::Eof) => break,
            Err(FillError::Abort) => break,
            Err(err) => return Err(anyhow!("{err}")),
        };

        if sender.send(request).await.is_err() {
            break;
        }

        // Collect the responses that have already arrived.
        while let Ok(msg) = rx.try_recv() {
            messages.push(msg);
        }
    }

    drop(sender);

    while let Some(msg) = rx.recv().await {
        messages.push(msg);
    }

    let (trailers, status) = match reader {
        Some(reader) => reader
            .await
            .unwrap_or_else(|_| (tonic::metadata::MetadataMap::new(), Status::ok(""))),
        None => (tonic::metadata::MetadataMap::new(), result.status),
    };
    Ok(response::Response {
        headers: result.headers,
        messages,
        trailers,
        status,
    })
}

/// Fills a request message.
fn fill<P: Prompter + ?Sized>(
    filler: &mut Filler<'_, P>,
    method: &MethodDescriptor,
) -> Result<crate::message::DynamicMessage, FillError> {
    filler.fill(&method.input())
}
