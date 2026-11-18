pub mod command;
pub mod completer;
pub mod prompt;
pub mod session;
pub mod table;

use anyhow::{Result, anyhow};
use std::collections::{BTreeSet, HashMap};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};

use crate::config::{Config, ServerConfig};
use crate::grpc::{Client, Headers};
use crate::idl::Idl;
use crate::message::filler::{FillError, Prompter};
use crate::present::response::ResponseSink;

use completer::{Completer, CompletionContext};
use session::{Session, TerminalSession};

/// The name of the type of a field, like `repl/proto_commands.go` does.
fn type_name(field: &prost_reflect::FieldDescriptor) -> String {
    use prost_reflect::Kind;

    match field.kind() {
        Kind::Message(desc) if field.is_map() => {
            let key = desc.map_entry_key_field();
            let value = desc.map_entry_value_field();
            format!("map<{}, {}>", type_name(&key), type_name(&value))
        }
        Kind::Message(desc) => format!("message ({})", desc.name()),
        Kind::Enum(desc) => format!("enum ({})", desc.name()),
        Kind::Double => "double".to_string(),
        Kind::Float => "float".to_string(),
        Kind::Int64 => "int64".to_string(),
        Kind::Uint64 => "uint64".to_string(),
        Kind::Int32 => "int32".to_string(),
        Kind::Uint32 => "uint32".to_string(),
        Kind::Sint32 => "sint32".to_string(),
        Kind::Sint64 => "sint64".to_string(),
        Kind::Fixed32 => "fixed32".to_string(),
        Kind::Fixed64 => "fixed64".to_string(),
        Kind::Sfixed32 => "sfixed32".to_string(),
        Kind::Sfixed64 => "sfixed64".to_string(),
        Kind::Bool => "bool".to_string(),
        Kind::String => "string".to_string(),
        Kind::Bytes => "bytes".to_string(),
    }
}

/// The outcome of a command.
pub enum Outcome {
    /// The REPL continues to run.
    Continue,
    /// The REPL exits.
    Exit,
}

/// A renderer that writes the response of a call with a sink.
struct SinkRenderer<'a, W: Write> {
    sink: &'a mut crate::present::response::CurlSink<W>,
}

impl<W: Write> crate::app::repl::ResponseRenderer for SinkRenderer<'_, W> {
    fn render(&mut self, event: crate::app::repl::ResponseEvent<'_>) -> Result<()> {
        match event {
            crate::app::repl::ResponseEvent::Header(headers) => self.sink.header(headers),
            crate::app::repl::ResponseEvent::Message(message) => self.sink.message(message),
        }
    }
}

/// The options of a call of the REPL.
#[derive(Debug, Clone, Copy, Default)]
pub struct CallOptions {
    /// The response contains the headers, the trailers and the status.
    pub enrich: bool,
    /// The response contains the fields with their default values.
    pub emit_defaults: bool,
    /// The prompt asks whether to dig down into a message field.
    pub dig_manually: bool,
    /// The bytes input is interpreted as a base64-encoded string.
    pub bytes_as_base64: bool,
    /// The bytes input is interpreted as a quoted byte literal.
    pub bytes_as_quoted_literals: bool,
    /// The bytes input is interpreted as a path to a file.
    pub bytes_from_file: bool,
    /// The prompt asks whether to add a value to a repeated field.
    pub add_repeated_manually: bool,
    /// The previous request of the method is repeated.
    pub repeat: bool,
}

/// The state of the REPL session.
pub struct State {
    /// The headers that are sent with each request.
    pub headers: Headers,
    /// The previous request of each method, used by the `--repeat` option.
    pub requests: HashMap<String, crate::message::DynamicMessage>,
}

/// The REPL environment.
pub struct Repl<'a> {
    cfg: &'a Config,
    idl: &'a Idl,
    client: &'a mut Client,
    state: State,
    session: Box<dyn Session>,
    out: Box<dyn Write>,
    err: Box<dyn Write>,
    /// The context of the completion, which is shared with the completer of the
    /// session.
    completion: Arc<RwLock<CompletionContext>>,
}

impl<'a> Repl<'a> {
    /// Creates a new REPL.
    pub fn new(cfg: &'a Config, idl: &'a Idl, client: &'a mut Client) -> Result<Self> {
        let completion = Arc::new(RwLock::new(CompletionContext::default()));
        let completer = Completer::new(idl.clone(), completion.clone());
        let session = TerminalSession::new(
            Box::new(completer),
            history_size(cfg),
            cfg.repl.colored_output,
        )?;

        let repl = Self {
            cfg,
            idl,
            client,
            state: State {
                headers: Headers::new(cfg.request.header.clone()),
                requests: HashMap::new(),
            },
            session: Box::new(session),
            out: Box::new(std::io::stdout()),
            err: Box::new(std::io::stderr()),
            completion,
        };

        repl.sync_completion_context();

        Ok(repl)
    }

    /// Creates a new REPL that reads the input from the given session.
    pub fn with_session(
        cfg: &'a Config,
        idl: &'a Idl,
        client: &'a mut Client,
        session: Box<dyn Session>,
    ) -> Result<Self> {
        let repl = Self {
            cfg,
            idl,
            client,
            state: State {
                headers: Headers::new(cfg.request.header.clone()),
                requests: HashMap::new(),
            },
            session,
            out: Box::new(std::io::stdout()),
            err: Box::new(std::io::stderr()),
            completion: Arc::new(RwLock::new(CompletionContext::default())),
        };

        repl.sync_completion_context();

        Ok(repl)
    }

    /// Creates a new REPL that writes the output to the given writer.
    pub fn with_output(
        cfg: &'a Config,
        idl: &'a Idl,
        client: &'a mut Client,
        session: Box<dyn Session>,
        out: Box<dyn Write>,
    ) -> Result<Self> {
        let mut repl = Self::with_session(cfg, idl, client, session)?;
        repl.out = out;
        Ok(repl)
    }

    /// Creates a new REPL that writes the output and the errors to the given writers.
    pub fn with_writers(
        cfg: &'a Config,
        idl: &'a Idl,
        client: &'a mut Client,
        session: Box<dyn Session>,
        out: Box<dyn Write>,
        err: Box<dyn Write>,
    ) -> Result<Self> {
        let mut repl = Self::with_session(cfg, idl, client, session)?;
        repl.out = out;
        repl.err = err;
        Ok(repl)
    }

    /// Runs the read-eval-print loop.
    pub async fn run(&mut self) -> Result<()> {
        if !self.cfg.repl.silent {
            self.print_splash()?;
        }

        loop {
            let prefix = self.make_prefix();
            let line = match self.session.read_line(&prefix) {
                Ok(Some(line)) => line,
                Ok(None) => break,
                Err(err) => return Err(err.into()),
            };

            let line = line.trim();
            if line.is_empty() {
                continue;
            }

            match self.run_line(line).await {
                Ok(Outcome::Continue) => {}
                Ok(Outcome::Exit) => break,
                Err(err) => writeln!(self.err, "{}", format_error(&err))?,
            }
            writeln!(self.out)?;
        }

        if !self.cfg.repl.silent {
            writeln!(self.out, "Good Bye :)")?;
        }

        Ok(())
    }

    /// Runs a command line.
    async fn run_line(&mut self, line: &str) -> Result<Outcome> {
        let (name, args) = match command::split(line)? {
            Some(parts) => parts,
            None => return Ok(Outcome::Continue),
        };

        if name == "help" {
            write!(self.out, "{}", command::help_text())?;
            return Ok(Outcome::Continue);
        }

        let cmd = command::lookup(&name).ok_or_else(|| anyhow!("unknown command '{name}'"))?;

        if let Some(help) = args.first()
            && (help == "-h" || help == "--help")
        {
            write!(self.out, "{}", cmd.help)?;
            return Ok(Outcome::Continue);
        }

        let parsed = cmd.parse(&args)?;
        match parsed {
            command::Parsed::Call {
                method,
                enrich,
                emit_defaults,
                dig_manually,
                bytes_as_base64,
                bytes_as_quoted_literals,
                bytes_from_file,
                add_repeated_manually,
                repeat,
            } => {
                let options = CallOptions {
                    enrich,
                    emit_defaults,
                    dig_manually,
                    bytes_as_base64,
                    bytes_as_quoted_literals,
                    bytes_from_file,
                    add_repeated_manually,
                    repeat,
                };
                self.call(method.as_deref(), options).await?;
                Ok(Outcome::Continue)
            }
            command::Parsed::Show(target) => {
                let out = self.show(&target)?;
                write!(self.out, "{out}")?;
                Ok(Outcome::Continue)
            }
            command::Parsed::Desc(symbol) => {
                let out = self.describe(&symbol)?;
                writeln!(self.out, "{out}")?;
                Ok(Outcome::Continue)
            }
            command::Parsed::Header(headers) => {
                for (key, value) in headers {
                    if value.is_empty() {
                        self.state.headers.remove(&key);
                        continue;
                    }
                    self.state.headers.add(&key, &value)?;
                }
                self.sync_completion_context();
                Ok(Outcome::Continue)
            }
            command::Parsed::Exit => Ok(Outcome::Exit),
        }
    }

    /// Keeps the context of the completion in sync with the state of the REPL.
    fn sync_completion_context(&self) {
        let Ok(mut context) = self.completion.write() else {
            return;
        };
        context.header_keys = self
            .state
            .headers
            .keys()
            .into_iter()
            .map(|key| key.to_string())
            .collect();
    }

    /// Formats the resources of the given target.
    fn show(&self, target: &str) -> Result<String> {
        match target.to_lowercase().as_str() {
            "p" | "package" | "packages" => {
                let packages: Vec<String> = self.idl.list_packages();
                Ok(table::format_table(
                    &["package".to_string()],
                    &packages.iter().map(|p| vec![p.clone()]).collect::<Vec<_>>(),
                ))
            }
            "s" | "svc" | "service" | "services" => {
                let services = self.idl.list_services();
                Ok(table::format_table(
                    &["name".to_string()],
                    &services.iter().map(|s| vec![s.clone()]).collect::<Vec<_>>(),
                ))
            }
            "m" | "msg" | "message" | "messages" => {
                let messages = self.list_message_names();
                Ok(table::format_table(
                    &["message".to_string()],
                    &messages.iter().map(|m| vec![m.clone()]).collect::<Vec<_>>(),
                ))
            }
            "a" | "r" | "rpc" | "api" => {
                let methods = self.list_methods();
                let rows = methods
                    .iter()
                    .map(|m| {
                        vec![
                            m.name().to_string(),
                            m.full_name().to_string(),
                            m.input().full_name().to_string(),
                            m.output().full_name().to_string(),
                        ]
                    })
                    .collect::<Vec<_>>();
                Ok(table::format_table(
                    &[
                        "name".to_string(),
                        "fully-qualified name".to_string(),
                        "request type".to_string(),
                        "response type".to_string(),
                    ],
                    &rows,
                ))
            }
            "h" | "header" | "headers" => {
                let mut rows = Vec::new();
                for key in self.state.headers.keys() {
                    for value in self.state.headers.get(key).unwrap_or_default() {
                        rows.push(vec![key.to_string(), value.clone()]);
                    }
                }
                Ok(table::format_table(
                    &["key".to_string(), "val".to_string()],
                    &rows,
                ))
            }
            _ => Err(anyhow!("unknown target '{target}'")),
        }
    }

    /// Describes the fields of a message, like `repl/proto_commands.go` does.
    ///
    /// Unlike the `desc` command of the CLI mode, the name of the message is relative
    /// to the selected package, like `usecase.GetTypeDescriptor` of the original does.
    /// A fully-qualified name is resolved as it is.
    fn describe(&self, symbol: &str) -> Result<String> {
        let desc = self
            .idl
            .resolve_message(symbol)
            .map_err(|e| anyhow!("failed to get the type descriptor of '{symbol}': {e}"))?;

        let mut rows: Vec<Vec<String>> = desc
            .fields()
            .map(|field| {
                vec![
                    field.name().to_string(),
                    type_name(&field),
                    field.is_list().to_string(),
                ]
            })
            .collect();
        rows.sort_by(|a, b| a[0].cmp(&b[0]));

        Ok(table::format_table(
            &[
                "field".to_string(),
                "type".to_string(),
                "repeated".to_string(),
            ],
            &rows,
        ))
    }

    /// Lists the names of the messages that are used by the loaded services.
    fn list_message_names(&self) -> Vec<String> {
        let mut names: BTreeSet<String> = BTreeSet::new();
        for method in self.list_methods() {
            for desc in [method.input(), method.output()] {
                names.insert(desc.name().to_string());
            }
        }
        names.into_iter().collect()
    }

    /// Lists the methods of all the loaded services.
    fn list_methods(&self) -> Vec<prost_reflect::MethodDescriptor> {
        let mut methods = Vec::new();
        for service in self.idl.list_services() {
            if let Ok(mut svc_methods) = self.idl.methods(&service) {
                methods.append(&mut svc_methods);
            }
        }
        methods
    }

    /// Calls the method.
    ///
    /// The method is selected from the loaded services if its name is omitted.
    async fn call(&mut self, method: Option<&str>, options: CallOptions) -> Result<()> {
        let mut prompter = self.session.prompter();

        let desc = match method {
            Some(name) => self.resolve_method(name)?,
            None => self.select_method(prompter.as_mut())?,
        };

        let fill_options = crate::message::filler::FillOptions {
            dig_manually: options.dig_manually,
            bytes_as_base64: options.bytes_as_base64,
            bytes_as_quoted_literals: options.bytes_as_quoted_literals,
            bytes_from_file: options.bytes_from_file,
            add_repeated_manually: options.add_repeated_manually,
        };

        let mut filler = crate::message::filler::Filler::new(
            prompter.as_mut(),
            fill_options,
            self.cfg.repl.input_prompt_format.clone(),
        );

        let previous = self.state.requests.get(desc.full_name()).cloned();

        // The response is rendered as it arrives, like the original does.
        let mut sink = crate::present::response::CurlSink::new(
            &mut self.out,
            options.enrich,
            options.emit_defaults,
        );
        let result = {
            let mut render = SinkRenderer { sink: &mut sink };
            let call_options = crate::app::repl::CallOptions {
                headers: self.state.headers.clone(),
                previous: previous.as_ref(),
                repeat: options.repeat,
                on_response: Some(&mut render),
            };
            crate::app::repl::repl_call(self.client, &desc, &mut filler, call_options).await?
        };
        sink.trailer(&result.response.trailers)?;
        sink.status(&result.response.status)?;

        // Remember the request so that it can be repeated later.
        if let Some(request) = &result.request {
            self.state
                .requests
                .insert(desc.full_name().to_string(), request.clone());
        }

        // A call that failed is reported after its response, like the original does.
        if result.response.status.code() != tonic::Code::Ok {
            return Err(crate::grpc::status_error(result.response.status));
        }

        Ok(())
    }

    /// Resolves the name of a method.
    ///
    /// A fully-qualified name is resolved as it is, while a short name is resolved
    /// among the methods of the loaded services.
    fn resolve_method(&self, name: &str) -> Result<prost_reflect::MethodDescriptor> {
        self.idl
            .resolve_method(name)
            .map_err(|e| anyhow!("failed to get the RPC descriptor for: {name}: {e}"))
    }

    /// Selects a method from the loaded services.
    fn select_method(
        &self,
        prompter: &mut dyn Prompter,
    ) -> Result<prost_reflect::MethodDescriptor> {
        let methods = self.list_methods();
        if methods.is_empty() {
            return Err(anyhow!("no RPC is available"));
        }

        let options: Vec<String> = methods
            .iter()
            .map(|method| {
                format!(
                    "{} ({})",
                    method.full_name(),
                    format_args!(
                        "{} -> {}",
                        method.input().full_name(),
                        method.output().full_name()
                    )
                )
            })
            .collect();

        let idx = match prompter.select_opt("select a method", &options) {
            Ok(Some(idx)) => idx,
            // The selection was canceled, either with ESC or with ctrl+c.
            Ok(None) | Err(FillError::Abort) => return Err(anyhow!("inputting canceled")),
            Err(err) => return Err(anyhow!("{err}")),
        };

        methods
            .get(idx)
            .cloned()
            .ok_or_else(|| anyhow!("no method is selected"))
    }

    /// Makes the prompt of the REPL.
    fn make_prefix(&self) -> String {
        build_prefix(&self.cfg.repl.prompt_format, &self.cfg.server)
    }

    /// Prints the splash text.
    fn print_splash(&mut self) -> Result<()> {
        let path = &self.cfg.repl.splash_text_path;
        if path.is_empty() {
            write!(self.out, "{}", prompt::SPLASH_TEXT)?;
            return Ok(());
        }
        let path = match path.strip_prefix("~/") {
            Some(rest) => {
                let home = std::env::var("HOME").unwrap_or_default();
                Path::new(&home).join(rest)
            }
            None => PathBuf::from(path),
        };
        if let Ok(content) = std::fs::read_to_string(&path) {
            write!(self.out, "{content}")?;
        }
        Ok(())
    }
}

/// Formats an error like the REPL does.
pub fn format_error(err: &anyhow::Error) -> String {
    format!("{err:#}")
}

/// The number of the entries that the history of the REPL remembers.
fn history_size(cfg: &Config) -> usize {
    usize::try_from(cfg.repl.history_size).unwrap_or(0)
}

/// Builds the prompt of the REPL from the configured format.
fn build_prefix(format: &str, server: &ServerConfig) -> String {
    // The package and the service are not selected anymore, so their placeholders are
    // replaced with nothing. They are kept for the sake of the older configurations.
    let prefix = format
        .replace("{package}.{service}", "")
        .replace("{package}", "")
        .replace("{service}", "")
        .replace("{dsn}", "")
        .replace("{addr}", &server.host)
        .replace("{port}", &server.port);

    format!("{prefix}> ")
}

/// The prompt of the REPL.
pub type ReplPrompt = prompt::InputPrompt;

#[cfg(test)]
mod tests {
    use super::*;

    fn server() -> ServerConfig {
        ServerConfig {
            host: "127.0.0.1".to_string(),
            port: "50051".to_string(),
            ..Default::default()
        }
    }

    #[test]
    fn builds_the_prompt_of_the_repl() {
        assert_eq!(
            build_prefix("{addr}:{port}", &server()),
            "127.0.0.1:50051> "
        );
    }

    #[test]
    fn builds_the_prompt_of_a_custom_format() {
        assert_eq!(
            build_prefix("{addr} -> {port}", &server()),
            "127.0.0.1 -> 50051> "
        );
    }

    #[test]
    fn the_placeholders_of_the_removed_service_are_replaced_with_nothing() {
        // The older configurations are kept working, but they show nothing.
        assert_eq!(
            build_prefix("{package}.{service}@{addr}:{port}", &server()),
            "@127.0.0.1:50051> "
        );
    }
}
