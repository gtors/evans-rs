//! The command table of the REPL, mirroring `repl/commands.go` of the original.

/// The commands of the REPL.
pub const COMMANDS: &[Command] = &[
    Command {
        name: "call",
        synopsis: "call a RPC",
        help: "usage: call [<method name>]\n\nThe method is selected from the loaded services if it is omitted.",
        flags: &[
            Flag {
                name: "enrich",
                short: None,
                usage: "enrich response output includes header, message, trailer and status",
            },
            Flag {
                name: "dig-manually",
                short: None,
                usage: "prompt asks whether to dig down if it encountered to a message field",
            },
            Flag {
                name: "bytes-as-base64",
                short: None,
                usage: "explicitly interpret TYPE_BYTES input as base64-encoded string",
            },
            Flag {
                name: "bytes-as-quoted-literals",
                short: None,
                usage: "interpret TYPE_BYTES input as a string of (quoted) byte literal or Unicode",
            },
            Flag {
                name: "bytes-from-file",
                short: None,
                usage: "interpret TYPE_BYTES input as a relative path to a file",
            },
            Flag {
                name: "emit-defaults",
                short: None,
                usage: "render fields with default values",
            },
            Flag {
                name: "repeat",
                short: Some("r"),
                usage: "repeat previous unary or server streaming request (if exists)",
            },
            Flag {
                name: "add-repeated-manually",
                short: None,
                usage: "prompt asks whether to add a value if it encountered to a repeated field",
            },
        ],
    },
    Command {
        name: "desc",
        synopsis: "describe the structure of selected message",
        help: "usage: desc <message name>",
        flags: &[],
    },
    Command {
        name: "exit",
        synopsis: "exit current REPL",
        help: "usage: exit",
        flags: &[],
    },
    Command {
        name: "header",
        synopsis: "set/unset headers to each request. if header value is empty, the header is removed.",
        help: "usage: header [options ...] <key>=<value>[, <key>=<value>...]",
        flags: &[Flag {
            name: "raw",
            short: Some("r"),
            usage: "treat the value as a raw string",
        }],
    },
    Command {
        name: "show",
        synopsis: "show package, service or RPC names",
        help: "usage: show <package | service | message | rpc | header>",
        flags: &[],
    },
];

/// A flag of a command.
pub struct Flag {
    pub name: &'static str,
    /// The short form of the flag, if it has one.
    pub short: Option<&'static str>,
    pub usage: &'static str,
}

/// A command of the REPL.
pub struct Command {
    pub name: &'static str,
    pub synopsis: &'static str,
    pub help: &'static str,
    pub flags: &'static [Flag],
}

/// The aliases of the commands.
pub const ALIASES: &[(&str, &str)] = &[("quit", "exit")];

/// Looks up a command by its name.
pub fn lookup(name: &str) -> Option<&'static Command> {
    if let Some(cmd) = COMMANDS.iter().find(|c| c.name == name) {
        return Some(cmd);
    }
    let alias = ALIASES.iter().find(|(a, _)| *a == name)?;
    COMMANDS.iter().find(|c| c.name == alias.1)
}

/// The parsed arguments of a command.
#[derive(Debug)]
pub enum Parsed {
    Call {
        /// The name of the method to call. It is selected interactively if it is omitted.
        method: Option<String>,
        enrich: bool,
        emit_defaults: bool,
        dig_manually: bool,
        bytes_as_base64: bool,
        bytes_as_quoted_literals: bool,
        bytes_from_file: bool,
        add_repeated_manually: bool,
        repeat: bool,
    },
    Show(String),
    Desc(String),
    Header(Vec<(String, String)>),
    Exit,
}

impl Command {
    /// Parses the arguments of the command.
    pub fn parse(&self, args: &[String]) -> anyhow::Result<Parsed> {
        parse(self, args)
    }
}

/// Parses the arguments of a command.
pub fn parse(cmd: &Command, args: &[String]) -> anyhow::Result<Parsed> {
    match cmd.name {
        "call" => parse_call(args),
        "show" => {
            let target = args
                .first()
                .ok_or_else(|| anyhow::anyhow!("argument required"))?;
            Ok(Parsed::Show(target.clone()))
        }
        "desc" => {
            let symbol = args
                .first()
                .ok_or_else(|| anyhow::anyhow!("argument required"))?;
            Ok(Parsed::Desc(symbol.clone()))
        }
        "header" => parse_header(args),
        "exit" => Ok(Parsed::Exit),
        _ => anyhow::bail!("unknown command '{}'", cmd.name),
    }
}

/// Splits a flag argument into its name and its optional value.
///
/// The long form is `--name` or `--name=value`. The short form is `-n` and is resolved
/// with the short names of the command.
fn split_flag(command: &str, arg: &str) -> anyhow::Result<Option<(String, Option<String>)>> {
    if let Some(flag) = arg.strip_prefix("--") {
        let (name, value) = match flag.split_once('=') {
            Some((name, value)) => (name.to_string(), Some(value.to_string())),
            None => (flag.to_string(), None),
        };
        return Ok(Some((name, value)));
    }

    if let Some(flag) = arg.strip_prefix('-')
        && !flag.is_empty()
    {
        return match long_flag_name(command, flag) {
            Some(name) => Ok(Some((name.to_string(), None))),
            None => anyhow::bail!("unknown flag: -{flag}"),
        };
    }

    Ok(None)
}

/// The long name of the flag of a command that has the given short name.
fn long_flag_name(command: &str, short: &str) -> Option<&'static str> {
    match (command, short) {
        ("call", "r") => Some("repeat"),
        ("header", "r") => Some("raw"),
        _ => None,
    }
}

fn parse_call(args: &[String]) -> anyhow::Result<Parsed> {
    let mut enrich = false;
    let mut emit_defaults = false;
    let mut dig_manually = false;
    let mut bytes_as_base64 = false;
    let mut bytes_as_quoted_literals = false;
    let mut bytes_from_file = false;
    let mut add_repeated_manually = false;
    let mut repeat = false;
    let mut method: Option<String> = None;

    let mut args = args.to_vec();
    while !args.is_empty() {
        let arg = args.remove(0);
        if let Some((name, value)) = split_flag("call", &arg)? {
            let enabled = match value {
                Some(value) => value != "false",
                None => true,
            };
            match name.as_str() {
                "enrich" => enrich = enabled,
                "emit-defaults" => emit_defaults = enabled,
                "dig-manually" => dig_manually = enabled,
                "bytes-as-base64" => bytes_as_base64 = enabled,
                "bytes-as-quoted-literals" => bytes_as_quoted_literals = enabled,
                "bytes-from-file" => bytes_from_file = enabled,
                "add-repeated-manually" => add_repeated_manually = enabled,
                "repeat" => repeat = enabled,
                _ => anyhow::bail!("unknown flag: --{name}"),
            }
            continue;
        }
        if method.is_some() {
            anyhow::bail!("too many arguments");
        }
        method = Some(arg);
    }

    if bytes_from_file && (bytes_as_base64 || bytes_as_quoted_literals) {
        anyhow::bail!("only one of --bytes-from-file or --bytes-as-* can be specified");
    }
    if bytes_as_base64 && bytes_as_quoted_literals {
        anyhow::bail!(
            "only one of --bytes-as-base64 or --bytes-as-quoted-literals can be specified"
        );
    }

    Ok(Parsed::Call {
        method,
        enrich,
        emit_defaults,
        dig_manually,
        bytes_as_base64,
        bytes_as_quoted_literals,
        bytes_from_file,
        add_repeated_manually,
        repeat,
    })
}

fn parse_header(args: &[String]) -> anyhow::Result<Parsed> {
    let mut raw = false;
    let mut headers = Vec::new();

    for arg in args {
        if let Some((name, value)) = split_flag("header", arg)? {
            match name.as_str() {
                "raw" if value.is_none() => raw = true,
                "raw" => anyhow::bail!("the --raw flag does not accept a value"),
                _ => anyhow::bail!("unknown flag: --{name}"),
            }
            continue;
        }

        let (key, value) = match arg.split_once('=') {
            Some((key, value)) => (key.to_string(), value.to_string()),
            None => (arg.clone(), String::new()),
        };

        if value.is_empty() {
            headers.push((key, String::new()));
            continue;
        }

        if raw {
            headers.push((key, value));
            continue;
        }

        for value in value.split(',') {
            headers.push((key.clone(), value.to_string()));
        }
    }

    Ok(Parsed::Header(headers))
}

/// Splits a command line into the command name and its arguments.
///
/// It returns an error when the line has an unterminated quote, so that the user is
/// told about it instead of the command being silently ignored.
pub fn split(line: &str) -> anyhow::Result<Option<(String, Vec<String>)>> {
    let mut args = Vec::new();
    let mut current = String::new();
    let mut quote: Option<char> = None;
    let mut has_token = false;

    for c in line.chars() {
        match quote {
            Some(q) if c == q => {
                quote = None;
            }
            Some(_) => current.push(c),
            None if c == '\'' || c == '"' => {
                quote = Some(c);
                has_token = true;
            }
            None if c.is_whitespace() => {
                if has_token {
                    args.push(std::mem::take(&mut current));
                    has_token = false;
                }
            }
            None => {
                current.push(c);
                has_token = true;
            }
        }
    }

    if quote.is_some() {
        anyhow::bail!("unterminated quote in the command line");
    }
    if has_token {
        args.push(current);
    }
    if args.is_empty() {
        return Ok(None);
    }

    let name = args.remove(0);
    Ok(Some((name, args)))
}

/// The help text of the REPL.
pub fn help_text() -> String {
    let mut commands: Vec<&Command> = COMMANDS.iter().collect();
    commands.sort_by(|a, b| a.name.cmp(b.name));

    let max_len = commands.iter().map(|c| c.name.len()).max().unwrap_or(0);
    let mut text = String::new();
    for cmd in commands {
        text.push_str(&format!(
            "  {:<width$}    {}\n",
            cmd.name,
            cmd.synopsis,
            width = max_len
        ));
    }

    format!("\nAvailable commands:\n{text}Show more details:\n  <command> --help\n")
        .trim_end()
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_a_command_line() {
        let (name, args) = split("call Unary --enrich").unwrap().unwrap();
        assert_eq!(name, "call");
        assert_eq!(args, vec!["Unary", "--enrich"]);
    }

    #[test]
    fn splits_a_command_line_with_quotes() {
        let (name, args) = split("show 'my service'").unwrap().unwrap();
        assert_eq!(name, "show");
        assert_eq!(args, vec!["my service"]);
    }

    #[test]
    fn returns_none_for_an_empty_line() {
        assert!(split("   ").unwrap().is_none());
    }

    #[test]
    fn rejects_an_unterminated_quote() {
        let err = split("desc \"foo").unwrap_err();
        assert!(err.to_string().contains("unterminated quote"), "{err}");
    }

    #[test]
    fn parses_the_call_flags() {
        let cmd = lookup("call").unwrap();
        let parsed = parse(
            cmd,
            &[
                "Unary".to_string(),
                "--enrich".to_string(),
                "--emit-defaults".to_string(),
            ],
        )
        .unwrap();
        match parsed {
            Parsed::Call {
                method,
                enrich,
                emit_defaults,
                ..
            } => {
                assert_eq!(method.as_deref(), Some("Unary"));
                assert!(enrich);
                assert!(emit_defaults);
            }
            _ => panic!("unexpected parsed command"),
        }
    }

    #[test]
    fn rejects_conflicting_bytes_flags() {
        let cmd = lookup("call").unwrap();
        let err = parse(
            cmd,
            &[
                "Unary".to_string(),
                "--bytes-from-file".to_string(),
                "--bytes-as-base64".to_string(),
            ],
        )
        .unwrap_err();
        assert!(err.to_string().contains("only one of"));
    }

    #[test]
    fn parses_headers() {
        let cmd = lookup("header").unwrap();
        let parsed = parse(cmd, &["foo=bar,baz".to_string()]).unwrap();
        match parsed {
            Parsed::Header(headers) => {
                assert_eq!(
                    headers,
                    vec![
                        ("foo".to_string(), "bar".to_string()),
                        ("foo".to_string(), "baz".to_string())
                    ]
                );
            }
            _ => panic!("unexpected parsed command"),
        }
    }

    #[test]
    fn resolves_aliases() {
        assert_eq!(lookup("quit").unwrap().name, "exit");
        assert!(lookup("nope").is_none());
    }
}
