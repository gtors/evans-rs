use std::path::PathBuf;

use clap::{Args, Parser, Subcommand};

#[derive(Parser, Debug)]
#[command(
    name = "evans",
    version,
    about = "evans: universal gRPC client",
    disable_help_subcommand = true
)]
pub struct Cli {
    #[command(flatten)]
    pub common: Common,
    #[command(subcommand)]
    pub command: Option<Command>,
    /// proto files to load
    #[arg(value_name = "PROTO")]
    pub protos: Vec<String>,
}

impl Cli {
    /// Returns the common flags of the invocation.
    ///
    /// The flags may be passed either before or after the subcommand, so the flags of the
    /// root command and the ones of the subcommand are merged. The subcommand takes
    /// precedence over the root command.
    pub fn common(&self) -> Common {
        match &self.command {
            None => self.common.clone(),
            Some(Command::Repl { common }) => merge_common(&self.common, common),
            Some(Command::Cli { command, .. }) => match &**command {
                CliCommand::Call { common, .. }
                | CliCommand::List { common, .. }
                | CliCommand::Desc { common, .. } => merge_common(&self.common, common),
            },
        }
    }
}

/// Merges the common flags of the root command and the ones of a subcommand.
fn merge_common(root: &Common, sub: &Common) -> Common {
    Common {
        path: merged_vec(&root.path, &sub.path),
        proto: merged_vec(&root.proto, &sub.proto),
        host: sub.host.clone().or_else(|| root.host.clone()),
        port: sub.port.clone().or_else(|| root.port.clone()),
        unix_socket: sub.unix_socket.clone().or_else(|| root.unix_socket.clone()),
        proxy: sub.proxy.clone().or_else(|| root.proxy.clone()),
        header: merged_vec(&root.header, &sub.header),
        reflection: sub.reflection || root.reflection,
        tls: sub.tls || root.tls,
        insecure: sub.insecure || root.insecure,
        cacert: sub.cacert.clone().or_else(|| root.cacert.clone()),
        cert: sub.cert.clone().or_else(|| root.cert.clone()),
        certkey: sub.certkey.clone().or_else(|| root.certkey.clone()),
        servername: sub.servername.clone().or_else(|| root.servername.clone()),
        silent: sub.silent || root.silent,
    }
}

fn merged_vec(root: &[String], sub: &[String]) -> Vec<String> {
    let mut merged = root.to_vec();
    merged.extend(sub.iter().cloned());
    merged
}

#[derive(Subcommand, Debug)]
pub enum Command {
    /// REPL mode
    Repl {
        #[command(flatten)]
        common: Common,
    },
    /// CLI mode
    Cli {
        #[command(flatten)]
        common: Common,
        #[command(subcommand)]
        command: Box<CliCommand>,
    },
}

#[derive(Subcommand, Debug)]
pub enum CliCommand {
    /// call a method
    #[command(alias = "c")]
    Call {
        /// method name to call (e.g. "pkg.Service.Method")
        method: String,
        /// a script file that will be executed by CLI mode
        #[arg(short = 'f', long, value_name = "FILE")]
        file: Option<PathBuf>,
        /// enrich response output includes header, message, trailer and status
        #[arg(long)]
        enrich: bool,
        /// render fields with default values
        #[arg(long)]
        emit_defaults: bool,
        /// output format. one of "json" or "curl". "curl" is a curl-like format
        #[arg(short = 'o', long, value_name = "FORMAT", default_value = "curl")]
        output: String,
        #[command(flatten)]
        common: Common,
    },
    /// list services or methods
    #[command(alias = "ls", alias = "show")]
    List {
        /// output format. one of "json" or "name"
        #[arg(short = 'o', long, value_name = "FORMAT", default_value = "name")]
        output: String,
        /// fully-qualified service or method name
        dsn: Option<String>,
        #[command(flatten)]
        common: Common,
    },
    /// describe the descriptor of a symbol
    #[command(alias = "describe")]
    Desc {
        /// fully-qualified symbol name
        symbol: Option<String>,
        #[command(flatten)]
        common: Common,
    },
}

#[derive(Args, Debug, Default, Clone)]
pub struct Common {
    /// comma-separated proto file paths
    #[arg(long, value_name = "PATH", value_delimiter = ',')]
    pub path: Vec<String>,
    /// comma-separated proto file names
    #[arg(long, value_name = "PROTO", value_delimiter = ',')]
    pub proto: Vec<String>,
    /// gRPC server host
    #[arg(long, value_name = "HOST")]
    pub host: Option<String>,
    /// the path of the unix domain socket of the server
    ///
    /// It takes precedence over the host and the port.
    #[arg(long, value_name = "PATH")]
    pub unix_socket: Option<String>,
    /// the address of the proxy that the connection goes through
    ///
    /// The scheme of the address selects the protocol of the proxy: `http` for a
    /// proxy that tunnels with the HTTP CONNECT method, `socks5` and `socks4` for the
    /// SOCKS proxies. The credentials may be part of the address.
    #[arg(long, value_name = "URL")]
    pub proxy: Option<String>,
    /// gRPC server port
    #[arg(short = 'p', long, value_name = "PORT")]
    pub port: Option<String>,
    /// default headers set to each request (repeatable, e.g. --header foo=bar)
    #[arg(long, value_name = "KEY=VALUE")]
    pub header: Vec<String>,
    /// use gRPC server reflection
    #[arg(short = 'r', long)]
    pub reflection: bool,
    /// use a secure TLS connection
    #[arg(short = 't', long)]
    pub tls: bool,
    /// do not verify the certificate of the server
    ///
    /// The connection is still encrypted, but it is not authenticated. The CA
    /// certificate is ignored, because the verification is turned off.
    #[arg(long)]
    pub insecure: bool,
    /// the CA certificate file for verifying the server
    #[arg(long, value_name = "FILE")]
    pub cacert: Option<PathBuf>,
    /// the certificate file for mutual TLS auth. it must be provided with --certkey
    #[arg(long, value_name = "FILE")]
    pub cert: Option<PathBuf>,
    /// the private key file for mutual TLS auth. it must be provided with --cert
    #[arg(long, value_name = "FILE")]
    pub certkey: Option<PathBuf>,
    /// override the server name used to verify the hostname (ignored if --tls is disabled)
    #[arg(long, value_name = "NAME")]
    pub servername: Option<String>,
    /// hide redundant output
    #[arg(short = 's', long)]
    pub silent: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(args: &[&str]) -> Cli {
        Cli::try_parse_from(std::iter::once("evans").chain(args.iter().copied()))
            .expect("failed to parse the test args")
    }

    #[test]
    fn a_boolean_flag_does_not_consume_the_subcommand() {
        let cli = parse(&["-r", "cli", "list"]);
        assert!(cli.common().reflection);
        assert!(matches!(cli.command, Some(Command::Cli { .. })));
    }

    #[test]
    fn a_boolean_flag_is_false_when_it_is_not_given() {
        let cli = parse(&["cli", "list"]);
        assert!(!cli.common().reflection);
        assert!(!cli.common().tls);
        assert!(!cli.common().silent);
    }

    #[test]
    fn a_boolean_flag_does_not_accept_a_value() {
        assert!(Cli::try_parse_from(["evans", "--reflection=false", "cli", "list"]).is_err());
    }

    #[test]
    fn a_boolean_flag_is_merged_with_the_one_of_the_subcommand() {
        let cli = parse(&["--tls", "cli", "call", "--tls", "pkg.Svc.Method"]);
        assert!(cli.common().tls);
    }

    #[test]
    fn the_web_flag_is_not_supported() {
        assert!(Cli::try_parse_from(["evans", "--web", "cli", "list"]).is_err());
    }
}
