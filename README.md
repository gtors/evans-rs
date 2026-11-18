# evans

A Rust rewrite of [evans](https://github.com/ktr0731/evans), the universal gRPC client.
It keeps the concepts and the look of the original: an interactive REPL and a UNIX-friendly
CLI mode that reads JSON from stdin and prints JSON to stdout.

## Features

- **REPL mode** — a line-based prompt with completion, history and a small command set
  (`call`, `service`, `package`, `show`, `desc`, `header`, `exit`).
- **CLI mode** — one-shot calls for scripting, with `--enrich`, `--emit-defaults` and
  `curl`/`json` output formats.
- **IDL sources** — local `.proto` files or gRPC server reflection.
- **Dynamic messages** — requests and responses are built from the descriptors at runtime,
  so no code generation against your protos is needed.
- **Transport** — unary, client, server and bidirectional streaming, gzip, TLS with
  custom CAs and mutual authentication.

## Build

```console
$ cargo build --release
$ ./target/release/evans --help
```

Rust 1.85 or later is required (the crate uses edition 2024).

## Usage

### REPL mode

```console
$ evans --proto api/api.proto
  ______
 |  ____|
 | |__    __   __   __ _   _ __    ___
 |  __|   \ \ / /  / _. | | '_ \  / __|
 | |____   \ V /  | (_| | | | | | \__ \
 |______|   \_/    \__,_| |_| |_| |___|

 more expressive universal gRPC client

api.Example@127.0.0.1:50051> package api
api.Example@127.0.0.1:50051> service Example
api.Example@127.0.0.1:50051> call Unary
name (string) => ktr
{
  "message": "hello, ktr"
}
```

Use the arrow keys to complete command names, flags and arguments. `ctrl+c` skips the rest
of the fields of the current message and `ctrl+d` finishes the input.

### CLI mode

```console
$ echo '{"name": "ktr"}' | evans --proto api/api.proto cli call api.Example.Unary
{
  "message": "hello, ktr"
}

$ evans --proto api/api.proto cli list
api.Example

$ evans --proto api/api.proto cli desc api.Example.Unary
api.Example.Unary:
rpc Unary ( .api.SimpleRequest ) returns ( .api.SimpleResponse );
```

Errors are written to stderr and the exit code is `1`, so stdout stays clean for piping
into `jq`. A call that fails keeps that exit code: with `--enrich`, its response is
written to stdout before the error, like the original does.

### Common flags

| Flag | Description |
|------|-------------|
| `--package` | default package name |
| `--service` | default service name |
| `--path` | comma-separated proto file search paths |
| `--proto` | comma-separated proto file names |
| `--host` | gRPC server host |
| `--port`, `-p` | gRPC server port |
| `--header` | default header (`key=value`), repeatable |
| `--reflection`, `-r` | use gRPC server reflection |
| `--tls`, `-t` | use a secure TLS connection |
| `--cacert` | the CA certificate file |
| `--cert` / `--certkey` | the client certificate and key for mutual TLS |
| `--servername` | override the server name used to verify the hostname |
| `--silent`, `-s` | hide redundant output |

The boolean flags accept an explicit value only when it is attached with `=`
(e.g. `--reflection=false`), so that the following argument is never mistaken for the
value of the flag.

## Configuration

Evans reads its configuration from, in increasing order of priority:

1. the built-in defaults,
2. `~/.config/evans/config.toml` (written on the first run),
3. the project `.evans.toml` (the current directory or the git root),
4. `EVANS_*` environment variables (`__` separates the levels, e.g. `EVANS_SERVER__PORT`),
5. the command line flags.

```toml
[default]
package = ""
protoPath = [""]
protoFile = [""]
service = ""

[meta]
configVersion = "0.1.0"

[repl]
promptFormat = "{package}.{service}@{addr}:{port}"
inputPromptFormat = "{ancestor}{name} ({type}) => "
coloredOutput = true
silent = false
splashTextPath = ""
historySize = 100

[server]
host = "127.0.0.1"
port = "50051"
reflection = false
tls = false
name = ""

[log]
prefix = "evans: "

[request]
caCertFile = ""
certFile = ""
certKeyFile = ""

[request.header]
grpc-client = ["evans"]
```

## Development

```console
$ cargo test          # unit tests and the end-to-end suite
$ cargo clippy --all-targets -- -D warnings
$ cargo fmt --check
```

The end-to-end suite starts an in-process gRPC server (see `tests/e2e.rs`) that serves the
test proto in `testdata/proto/test.proto`.

## Releases

The GitHub Actions workflows build the binary for Linux and package it for every
supported architecture:

| Target | Architecture | Package |
|--------|--------------|---------|
| `x86_64-unknown-linux-gnu` | x86_64 | `evans-<version>-linux-x86_64.tar.gz` |
| `x86_64-unknown-linux-musl` | x86_64 | `evans-<version>-linux-x86_64-musl.tar.gz` |

Every package contains the `evans` binary and its README, and it is published with a
SHA-256 checksum. Pushing a `v*` tag publishes the packages as a GitHub release.

## License

MIT
