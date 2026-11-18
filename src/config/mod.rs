use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use tokio::process::Command;

use anyhow::{Context, Result};
use figment::Figment;
use figment::providers::{Env, Format, Serialized, Toml};
use figment::value::Value;
use serde::{Deserialize, Serialize};

use crate::cli::{Cli, Common};

pub const LOCAL_CONFIG_NAME: &str = ".evans.toml";
pub const GLOBAL_CONFIG_NAME: &str = "config.toml";
const ENV_PREFIX: &str = "EVANS_";

#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub default: DefaultConfig,
    pub meta: MetaConfig,
    pub repl: ReplConfig,
    pub server: ServerConfig,
    pub log: LogConfig,
    pub request: RequestConfig,
}

#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct DefaultConfig {
    pub proto_path: Vec<String>,
    pub proto_file: Vec<String>,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct MetaConfig {
    pub config_version: String,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct ReplConfig {
    pub prompt_format: String,
    pub input_prompt_format: String,
    pub colored_output: bool,
    pub silent: bool,
    pub splash_text_path: String,
    pub history_size: i64,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct ServerConfig {
    pub host: String,
    pub port: String,
    /// The path of the unix domain socket of the server.
    ///
    /// It takes precedence over the host and the port.
    pub unix_socket: String,
    pub reflection: bool,
    pub tls: bool,
    pub name: String,
    /// The certificate of the server is not verified.
    pub insecure: bool,
    /// The address of the proxy that the connection goes through.
    ///
    /// The scheme of the address selects the protocol of the proxy: `http` for a
    /// proxy that tunnels with the HTTP CONNECT method, `socks5` and `socks4` for the
    /// SOCKS proxies.
    pub proxy: String,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct LogConfig {
    pub prefix: String,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct RequestConfig {
    pub header: BTreeMap<String, Vec<String>>,
    pub ca_cert_file: String,
    pub cert_file: String,
    pub cert_key_file: String,
}

impl Default for MetaConfig {
    fn default() -> Self {
        Self {
            config_version: env!("CARGO_PKG_VERSION").to_string(),
        }
    }
}

impl Default for ReplConfig {
    fn default() -> Self {
        Self {
            prompt_format: "{addr}:{port}".to_string(),
            input_prompt_format: "{ancestor}{name} ({type}) => ".to_string(),
            colored_output: true,
            silent: false,
            splash_text_path: String::new(),
            history_size: 100,
        }
    }
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            host: "127.0.0.1".to_string(),
            port: "50051".to_string(),
            unix_socket: String::new(),
            reflection: false,
            tls: false,
            name: String::new(),
            insecure: false,
            proxy: String::new(),
        }
    }
}

impl Default for LogConfig {
    fn default() -> Self {
        Self {
            prefix: "evans: ".to_string(),
        }
    }
}

impl Default for RequestConfig {
    fn default() -> Self {
        let mut header = BTreeMap::new();
        header.insert("grpc-client".to_string(), vec!["evans".to_string()]);
        Self {
            header,
            ca_cert_file: String::new(),
            cert_file: String::new(),
            cert_key_file: String::new(),
        }
    }
}

#[derive(Debug, thiserror::Error)]
#[error("invalid config condition: {}", .0.join("; "))]
pub struct ValidationError(Vec<String>);

pub async fn load(cli: &Cli) -> Result<Config> {
    let global = global_config_path();
    write_default_config(&global).with_context(|| {
        format!(
            "failed to write the latest default config to {}",
            global.display()
        )
    })?;
    let local = local_config_path().await;
    let cfg = from_files(Some(&global), local.as_deref(), cli)?;
    Ok(cfg)
}

pub(crate) fn from_files(global: Option<&Path>, local: Option<&Path>, cli: &Cli) -> Result<Config> {
    let common = cli.common();
    // The providers are merged from the lowest to the highest priority: the defaults,
    // the config files, the environment variables and the command line flags.
    let mut figment = Figment::new();
    if let Some(global) = global {
        figment = figment.merge(Toml::file(global));
    }
    if let Some(local) = local {
        figment = figment.merge(Toml::file(local));
    }
    figment = figment.merge(
        Env::prefixed(ENV_PREFIX).map(|k| k.as_str().to_lowercase().replace("__", ".").into()),
    );
    for (key, value) in flag_overrides(&common) {
        figment = figment.merge(Serialized::default(key, value));
    }

    let mut cfg: Config = figment
        .extract()
        .context("failed to merge command line flags and config files")?;

    // The flags replace the values of the config files, like the original does. The
    // positional proto files are appended to them.
    if !common.path.is_empty() {
        cfg.default.proto_path = common.path.clone();
    }
    if !common.proto.is_empty() {
        cfg.default.proto_file = common.proto.clone();
    }
    cfg.default.proto_file.extend(cli.protos.iter().cloned());
    // The original stores the proto files and paths as a list that may start with an
    // empty entry. Such entries are dropped.
    cfg.default.proto_path.retain(|p| !p.is_empty());
    cfg.default.proto_file.retain(|p| !p.is_empty());
    let flag_headers = parse_headers(&common.header)?;
    for (k, v) in flag_headers {
        cfg.request.header.entry(k).or_default().extend(v);
    }
    // The boolean flags are switches, so they can only turn a setting on. They are
    // merged with the config instead of overriding it.
    cfg.server.reflection |= common.reflection;
    cfg.server.tls |= common.tls;
    cfg.server.insecure |= common.insecure;
    cfg.repl.silent |= common.silent;
    // The insecure connection is a TLS connection whose certificate is not verified.
    cfg.server.tls |= cfg.server.insecure;
    cfg.validate()?;
    Ok(cfg)
}

fn flag_overrides(common: &Common) -> Vec<(&'static str, Value)> {
    let mut out = Vec::new();
    if let Some(v) = &common.host {
        out.push(("server.host", Value::from(v.as_str())));
    }
    if let Some(v) = &common.port {
        out.push(("server.port", Value::from(v.as_str())));
    }
    if let Some(v) = &common.unix_socket {
        out.push(("server.unixSocket", Value::from(v.as_str())));
    }
    if let Some(v) = &common.proxy {
        out.push(("server.proxy", Value::from(v.as_str())));
    }
    if let Some(v) = &common.servername {
        out.push(("server.name", Value::from(v.as_str())));
    }
    if let Some(v) = &common.cacert {
        out.push(("request.caCertFile", Value::from(v.display().to_string())));
    }
    if let Some(v) = &common.cert {
        out.push(("request.certFile", Value::from(v.display().to_string())));
    }
    if let Some(v) = &common.certkey {
        out.push(("request.certKeyFile", Value::from(v.display().to_string())));
    }
    out
}

fn parse_headers(header: &[String]) -> Result<BTreeMap<String, Vec<String>>> {
    let mut out: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for pair in header {
        let (k, v) = pair
            .split_once('=')
            .ok_or_else(|| anyhow::anyhow!("{pair} must be formatted as key=value"))?;
        out.entry(k.to_string()).or_default().push(v.to_string());
    }
    Ok(out)
}

pub fn global_config_path() -> PathBuf {
    let mut path = dirs::config_dir().unwrap_or_else(|| PathBuf::from("."));
    path.push("evans");
    path.push(GLOBAL_CONFIG_NAME);
    path
}

async fn local_config_path() -> Option<PathBuf> {
    if Path::new(LOCAL_CONFIG_NAME).exists() {
        return Some(LOCAL_CONFIG_NAME.into());
    }
    let output = Command::new("git")
        .arg("rev-parse")
        .arg("--show-cdup")
        .output()
        .await
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let root = String::from_utf8(output.stdout).ok()?;
    let root = root.trim();
    if root.is_empty() {
        return Some(PathBuf::from(LOCAL_CONFIG_NAME));
    }
    let path = PathBuf::from(root).join(LOCAL_CONFIG_NAME);
    if path.exists() { Some(path) } else { None }
}

fn write_default_config(path: &Path) -> Result<()> {
    if path.exists() {
        return Ok(());
    }
    let content = toml::to_string_pretty(&Config::default())
        .context("failed to serialize the default config")?;
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).context("failed to create config dirs")?;
    }
    std::fs::write(path, content).context("failed to write the default config")
}

impl Config {
    pub fn validate(&self) -> Result<()> {
        let mut errors: Vec<String> = Vec::new();
        if self.server.port.is_empty() {
            errors.push("port must not be empty".to_string());
        }
        if self.request.cert_file.is_empty() && !self.request.cert_key_file.is_empty() {
            errors.push("certFile config or --cert flag required".to_string());
        }
        if !self.request.cert_file.is_empty() && self.request.cert_key_file.is_empty() {
            errors.push("certKeyFile config or --certkey flag required".to_string());
        }
        if self.default.proto_file.is_empty() && !self.server.reflection {
            errors.push("one or more proto files, or gRPC reflection required".to_string());
        }
        if errors.is_empty() {
            Ok(())
        } else {
            Err(ValidationError(errors).into())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use clap::Parser as _;

    fn cli(args: &[&str]) -> Cli {
        Cli::try_parse_from(std::iter::once("evans").chain(args.iter().copied()))
            .expect("failed to parse test args")
    }

    #[test]
    fn defaults() {
        let cfg = from_files(None, None, &cli(&["-r", "-p", "50051"])).unwrap();
        assert_eq!(cfg.server.host, "127.0.0.1");
        assert_eq!(cfg.server.port, "50051");
        assert!(cfg.server.reflection);
        assert_eq!(cfg.repl.prompt_format, "{addr}:{port}");
        assert_eq!(
            cfg.request.header.get("grpc-client"),
            Some(&vec!["evans".to_string()])
        );
    }

    #[test]
    fn global_config_file() {
        let dir = std::env::temp_dir().join(format!("evans-test-global-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("config.toml");
        std::fs::write(
            &path,
            "[server]\nhost = \"1.2.3.4\"\nport = \"1234\"\n[default]\nprotoFile = [\"a.proto\"]\n",
        )
        .unwrap();
        let cfg = from_files(Some(&path), None, &cli(&["-r"])).unwrap();
        assert_eq!(cfg.server.host, "1.2.3.4");
        assert_eq!(cfg.server.port, "1234");
        assert_eq!(cfg.default.proto_file, vec!["a.proto".to_string()]);
        assert_eq!(cfg.request.header.get("grpc-client").unwrap(), &["evans"]);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn flags_win_over_config_file() {
        let dir = std::env::temp_dir().join(format!("evans-test-flag-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("config.toml");
        std::fs::write(&path, "[server]\nhost = \"1.2.3.4\"\nport = \"1234\"\n").unwrap();
        let cfg = from_files(Some(&path), None, &cli(&["-r", "--host", "9.9.9.9"])).unwrap();
        assert_eq!(cfg.server.host, "9.9.9.9");
        assert_eq!(cfg.server.port, "1234");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn validation_errors() {
        let cfg = from_files(None, None, &cli(&[])).unwrap_err();
        assert!(
            cfg.to_string()
                .contains("one or more proto files, or gRPC reflection required")
        );
    }

    #[test]
    fn the_proto_flags_replace_the_config_files() {
        let dir = std::env::temp_dir().join(format!("evans-test-proto-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("config.toml");
        std::fs::write(
            &path,
            "[default]\nprotoPath = [\"from-config\"]\nprotoFile = [\"from-config.proto\"]\n",
        )
        .unwrap();

        let cfg = from_files(Some(&path), None, &cli(&["--proto", "from-flag.proto"])).unwrap();
        assert_eq!(cfg.default.proto_file, vec!["from-flag.proto"]);
        // The paths are not overridden, so the ones of the config file are kept.
        assert_eq!(cfg.default.proto_path, vec!["from-config"]);

        let cfg = from_files(Some(&path), None, &cli(&["-r"])).unwrap();
        assert_eq!(cfg.default.proto_file, vec!["from-config.proto"]);
        assert_eq!(cfg.default.proto_path, vec!["from-config"]);

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn the_header_flag_is_added_once() {
        let cfg = from_files(None, None, &cli(&["-r", "--header", "x-foo=bar"])).unwrap();
        assert_eq!(
            cfg.request.header.get("x-foo"),
            Some(&vec!["bar".to_string()])
        );
    }

    #[test]
    fn the_environment_variables_win_over_the_config_files() {
        let dir = std::env::temp_dir().join(format!("evans-test-env-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("config.toml");
        std::fs::write(&path, "[server]\nname = \"from-config\"\n").unwrap();

        // The name of the server is not used by the other tests, so the variable does
        // not interfere with them.
        // SAFETY: the tests of this module run in parallel, but no other test reads the
        // name of the server.
        unsafe { std::env::set_var("EVANS_SERVER__NAME", "from-env") };
        let cfg = from_files(Some(&path), None, &cli(&["-r"])).unwrap();
        unsafe { std::env::remove_var("EVANS_SERVER__NAME") };

        assert_eq!(cfg.server.name, "from-env");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn invalid_header_format() {
        let err = from_files(None, None, &cli(&["-r", "--header", "no-key"])).unwrap_err();
        assert!(err.to_string().contains("must be formatted as key=value"));
    }

    #[test]
    fn cert_pair_validation() {
        let err = from_files(None, None, &cli(&["-r", "--certkey", "k.pem"])).unwrap_err();
        assert!(
            err.to_string()
                .contains("certFile config or --cert flag required")
        );
    }
}
