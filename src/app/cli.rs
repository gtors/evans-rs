//! CLI mode: `evans cli call|list|desc`.

use std::io::Read;

use anyhow::{Context, Result, anyhow};
use prost_reflect::MethodDescriptor;

use crate::cli::CliCommand;
use crate::config::Config;
use crate::grpc::Client;
use crate::idl::Idl;
use crate::message;
use crate::present::{descriptor, response};

/// Runs a CLI command.
pub async fn run(cfg: &Config, idl: &Idl, command: &CliCommand) -> Result<()> {
    match command {
        CliCommand::Call {
            method,
            file,
            enrich,
            emit_defaults,
            output,
            ..
        } => {
            let method = resolve_method(idl, method)?;
            let body = read_body(file.as_deref())?;
            let mut client = Client::new(&cfg.server, &cfg.request).await?;
            let response = call(&mut client, &method, &body).await?;
            let out = match output.as_str() {
                "json" => response::format_json_response(&response, *enrich, *emit_defaults)?,
                _ => response::format_curl(&response, *enrich, *emit_defaults)?,
            };
            print!("{out}");
            // A call that failed is reported after its response, like the original does.
            if response.status.code() != tonic::Code::Ok {
                return Err(crate::grpc::status_error(response.status));
            }
            Ok(())
        }
        CliCommand::List { output, dsn, .. } => {
            let out = list(idl, dsn.as_deref(), output)?;
            print!("{out}");
            Ok(())
        }
        CliCommand::Desc { symbol, .. } => {
            let out = describe(idl, symbol.as_deref())?;
            print!("{out}");
            Ok(())
        }
    }
}

/// Resolves the method to call. The method name may be a fully-qualified method name
/// (`pkg.Service.Method`) or the short name of a method.
fn resolve_method(idl: &Idl, name: &str) -> Result<MethodDescriptor> {
    idl.resolve_method(name)
        .map_err(|e| anyhow!("failed to get the RPC descriptor for: {name}: {e}"))
}

/// Reads the request body from the file or from stdin.
fn read_body(file: Option<&std::path::Path>) -> Result<String> {
    match file {
        Some(path) => std::fs::read_to_string(path)
            .with_context(|| format!("failed to open the script file {}", path.display())),
        None => {
            let mut buf = String::new();
            std::io::stdin()
                .read_to_string(&mut buf)
                .context("failed to read the request body from stdin")?;
            Ok(buf)
        }
    }
}

/// Calls the method and collects the response.
async fn call(
    client: &mut Client,
    method: &MethodDescriptor,
    body: &str,
) -> Result<response::Response> {
    if method.is_client_streaming() {
        // Client streaming RPCs accept a stream of JSON documents, one per request.
        let messages = message::from_json_documents(&method.input(), body)?;
        let requests = futures::stream::iter(messages);
        let result = client.invoke_client_streaming(method, requests).await?;
        return Ok(response::Response {
            headers: result.headers,
            messages: result.message.into_iter().collect(),
            trailers: result.trailers,
            status: result.status,
        });
    }

    let request = message::from_json(&method.input(), body)?;

    if method.is_server_streaming() {
        let result = client.invoke_server_streaming(method, request).await?;
        let (messages, trailers, status) =
            crate::grpc::collect_stream(result.stream, result.status, None).await?;
        return Ok(response::Response {
            headers: result.headers,
            messages,
            trailers,
            status,
        });
    }

    let result = client.invoke(method, request).await?;
    Ok(response::Response {
        headers: result.headers,
        messages: result.message.into_iter().collect(),
        trailers: result.trailers,
        status: result.status,
    })
}

/// Lists services or methods.
fn list(idl: &Idl, dsn: Option<&str>, output: &str) -> Result<String> {
    let dsn = dsn.unwrap_or_default();

    if dsn.is_empty() {
        let services = idl.list_services();
        return match output {
            "json" => {
                let value = serde_json::json!({
                    "services": services.iter().map(|name| serde_json::json!({ "name": name })).collect::<Vec<_>>(),
                });
                let mut out = serde_json::to_string_pretty(&value)?;
                out.push('\n');
                Ok(out)
            }
            _ => Ok(format!("{}\n", crate::present::format_names(&services))),
        };
    }

    if let Ok((fqsn, method)) = idl.resolve_fully_qualified_method_name(dsn) {
        let desc = idl.method(&fqsn, &method)?;
        let entry = method_entry(&desc);
        return match output {
            "json" => {
                let mut out = serde_json::to_string_pretty(&entry)?;
                out.push('\n');
                Ok(out)
            }
            _ => Ok(format!("{}\n", entry.fully_qualified_name)),
        };
    }

    let methods = idl
        .methods(dsn)
        .map_err(|_| anyhow!("unknown fully-qualified service name or method name '{dsn}'"))?;
    let mut entries: Vec<MethodEntry> = methods.iter().map(method_entry).collect();
    entries.sort_by(|a, b| a.fully_qualified_name.cmp(&b.fully_qualified_name));

    match output {
        "json" => {
            let value = serde_json::json!({ "methods": entries });
            let mut out = serde_json::to_string_pretty(&value)?;
            out.push('\n');
            Ok(out)
        }
        _ => {
            let names: Vec<String> = entries
                .into_iter()
                .map(|e| e.fully_qualified_name)
                .collect();
            Ok(format!("{}\n", crate::present::format_names(&names)))
        }
    }
}

/// The JSON representation of a method, like `usecase/format_methods.go` does.
#[derive(Debug, serde::Serialize)]
struct MethodEntry {
    name: String,
    fully_qualified_name: String,
    request_type: String,
    response_type: String,
}

fn method_entry(method: &MethodDescriptor) -> MethodEntry {
    MethodEntry {
        name: method.name().to_string(),
        fully_qualified_name: method.full_name().to_string(),
        request_type: method.input().full_name().to_string(),
        response_type: method.output().full_name().to_string(),
    }
}

/// Describes the descriptor of a symbol.
fn describe(idl: &Idl, symbol: Option<&str>) -> Result<String> {
    let pool = idl.pool();

    match symbol {
        Some(symbol) => {
            let out = descriptor::format_descriptor(pool, symbol)
                .ok_or_else(|| anyhow!("failed to resolve symbol '{symbol}'"))?;
            Ok(format!("{out}\n"))
        }
        None => {
            let mut out: Vec<String> = Vec::new();
            for service in idl.list_services() {
                let desc = descriptor::format_descriptor(pool, &service)
                    .ok_or_else(|| anyhow!("failed to resolve symbol '{service}'"))?;
                out.push(desc);
            }
            Ok(format!("{}\n", out.join("\n\n")))
        }
    }
}
