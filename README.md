# evans

A Rust rewrite of [evans](https://github.com/ktr0731/evans), the universal gRPC client.
It keeps the concepts and the look of the original: an interactive REPL and a UNIX-friendly
CLI mode that reads JSON from stdin and prints JSON to stdout.

## Features

- **REPL mode** — a line-based prompt with completion, history and a small command set
  (`call`, `show`, `desc`, `header`, `exit`).
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

127.0.0.1:50051> call api.Example.Unary
name (string) => ktr
{
  "message": "hello, ktr"
}
```

Use `tab` to complete command names, flags and arguments. The candidates are read from
the loaded `.proto` files, so `call` completes the methods of the services and `desc`
completes the messages. `shift+tab` cycles through the candidates backwards and
`ctrl+space` opens the menu as well.

`call` without a method name opens a list of the methods of the loaded services, so
that the method can be picked instead of typed. `ctrl+c` skips the rest of the fields
of the current message and `ctrl+d` finishes the input.

The methods and the messages are addressed by their fully-qualified name, like
`api.Example.Unary`. A short name, like `Unary`, is resolved among the loaded services
and is ambiguous when several of them share it.

The fields of the `bytes` type are asked for in two steps: first the format of the
value is chosen, and then the value is typed. The `file` format reads the contents of
a file, whose path is completed with `tab` like the other paths.

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
| `--path` | comma-separated proto file search paths |
| `--proto` | comma-separated proto file names |
| `--host` | gRPC server host |
| `--unix-socket` | the path of the unix domain socket of the server |
| `--port`, `-p` | gRPC server port |
| `--header` | default header (`key=value`), repeatable |
| `--reflection`, `-r` | use gRPC server reflection |
| `--tls`, `-t` | use a secure TLS connection |
| `--insecure` | do not verify the certificate of the server (implies TLS) |
| `--proxy` | the address of the proxy that the connection goes through |
| `--cacert` | the CA certificate file |
| `--cert` / `--certkey` | the client certificate and key for mutual TLS |
| `--servername` | override the server name used to verify the hostname |
| `--silent`, `-s` | hide redundant output |

The boolean flags are switches: they take no value, so `--reflection` turns the
reflection on and the absence of the flag leaves it as the configuration says.

`--insecure` turns the verification of the certificate of the server off. The
connection is still encrypted, but it is not authenticated, so it is vulnerable to a
man-in-the-middle attack. The CA certificate is ignored in that mode.

`--unix-socket` connects to the server through a unix domain socket instead of a TCP
connection. It takes precedence over the host and the port, and TLS is not available
over it.

`--proxy` sends the connection through a proxy. The scheme of the address selects the
protocol of the proxy: `http` for a proxy that tunnels the connection with the HTTP
CONNECT method, `socks5` and `socks4` for the SOCKS proxies. The credentials of the
proxy may be part of the address, like `socks5://user:password@127.0.0.1:1080`. When
the address is not given, the `ALL_PROXY` and `HTTPS_PROXY` environment variables are
used. The TLS handshake happens over the tunnel of the proxy, so the certificate of
the server is verified against the server itself.

## Configuration

Evans reads its configuration from, in increasing order of priority:

1. the built-in defaults,
2. `~/.config/evans/config.toml` (written on the first run),
3. the project `.evans.toml` (the current directory or the git root),
4. `EVANS_*` environment variables (`__` separates the levels, e.g. `EVANS_SERVER__PORT`),
5. the command line flags.

```toml
[default]
protoPath = [""]
protoFile = [""]

[meta]
configVersion = "0.1.0"

[repl]
promptFormat = "{addr}:{port}"
inputPromptFormat = "{ancestor}{name} ({type}) => "
coloredOutput = true
silent = false
splashTextPath = ""
historySize = 100

[server]
host = "127.0.0.1"
port = "50051"
unixSocket = ""
reflection = false
tls = false
name = ""
insecure = false
proxy = ""

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

The common tasks are wrapped in a [justfile](https://github.com/casey/just), so
`just` lists them:

```console
$ just            # the checks of the CI: the linter and the tests
$ just test       # unit tests and the end-to-end suite
$ just test-e2e   # the end-to-end suite only
$ just lint       # cargo clippy --all-targets -- -D warnings
$ just fmt        # cargo fmt
$ just repl       # the REPL against the test proto
$ just ci         # the formatting, the linter and the tests
```

The same commands work without `just`:

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

| Target | Architecture | Packages |
|--------|--------------|----------|
| `x86_64-unknown-linux-gnu` | x86_64 | `evans-<version>-linux-x86_64.tar.gz`, `evans_<version>_amd64.deb`, `evans-<version>-1.x86_64.rpm`, `evans-<version>-1-x86_64.pkg.tar.zst` |
| `x86_64-unknown-linux-musl` | x86_64 | `evans-<version>-linux-x86_64-musl.tar.gz` |

Every package contains the `evans` binary and its README, and it is published with a
SHA-256 checksum. Pushing a `v*` tag publishes the packages as a GitHub release.

The native packages are built with [nfpm](https://nfpm.goreleaser.com) from
`packaging/nfpm.yaml`, so a single configuration describes the deb, the rpm and the
package for Arch Linux at once. The `package` workflow builds them on every push and
on every pull request that touches the sources, and attaches them to the release of a
`v*` tag.

## License

MIT
