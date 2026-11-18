//! The completer of the REPL, mirroring `repl/completer.go` of the original.

use std::collections::BTreeSet;

use reedline::{Completer as ReedlineCompleter, CompletionResult, Suggestion};

use crate::idl::Idl;

use super::command;

/// The completer of the REPL.
#[derive(Clone)]
pub struct Completer {
    idl: Idl,
}

impl Completer {
    /// Creates a new completer.
    pub fn new(idl: Idl) -> Self {
        Self { idl }
    }

    /// Returns the suggestions for the given input.
    pub fn suggestions(&self, line: &str, pos: usize) -> Vec<Suggestion> {
        let before = &line[..pos];
        if before.is_empty() {
            return Vec::new();
        }

        let args: Vec<&str> = before.split_whitespace().collect();
        // The word before the cursor is empty if the cursor is after a whitespace.
        let word = match before.rfind(char::is_whitespace) {
            Some(i) => &before[i + 1..],
            None => before,
        };
        let cmd_name = args.first().copied().unwrap_or("");

        let mut suggestions = Vec::new();

        let cmd = command::lookup(cmd_name);
        match cmd {
            None => {
                if args.len() == 1 {
                    suggestions.push(suggestion("help", "show help message"));
                    for cmd in command::COMMANDS {
                        suggestions.push(suggestion(cmd.name, cmd.synopsis));
                    }
                }
            }
            Some(cmd) => {
                if !cmd.flags.is_empty() && word.starts_with('-') {
                    for flag in cmd.flags {
                        suggestions.push(suggestion(&format!("--{}", flag.name), flag.usage));
                        if let Some(short) = flag.short {
                            suggestions.push(suggestion(&format!("-{short}"), flag.usage));
                        }
                    }
                    return filter(suggestions, word);
                }

                let rest = &args[1..];
                match cmd.name {
                    "show" if rest.len() <= 1 => {
                        suggestions.push(suggestion("package", "show loaded package names"));
                        suggestions.push(suggestion("service", "show loaded service names"));
                        suggestions.push(suggestion("message", "show loaded message names"));
                        suggestions.push(suggestion(
                            "rpc",
                            "show RPC names belonging to the current selected service",
                        ));
                        suggestions.push(suggestion(
                            "header",
                            "show headers which will be added to each request",
                        ));
                    }
                    "package" if rest.len() <= 1 => {
                        for pkg in self.idl.list_packages() {
                            suggestions.push(suggestion(&pkg, ""));
                        }
                    }
                    "service" if rest.len() <= 1 => {
                        for svc in self.idl.list_services() {
                            suggestions.push(suggestion(&svc, ""));
                        }
                    }
                    "call" if rest.len() <= 1 => {
                        for method in self.methods() {
                            suggestions.push(suggestion(method.name(), ""));
                        }
                    }
                    "desc" if rest.len() <= 1 => {
                        for name in self.message_names() {
                            suggestions.push(suggestion(&name, ""));
                        }
                    }
                    _ => {}
                }

                if !suggestions.is_empty() {
                    suggestions.push(suggestion("--help", "show command help message"));
                }
            }
        }

        filter(suggestions, word)
    }

    /// Lists the methods of all loaded services.
    fn methods(&self) -> Vec<prost_reflect::MethodDescriptor> {
        let mut methods = Vec::new();
        for service in self.idl.list_services() {
            if let Ok(mut svc_methods) = self.idl.methods(&service) {
                methods.append(&mut svc_methods);
            }
        }
        methods
    }

    /// Lists the names of the messages that are used by the loaded services.
    fn message_names(&self) -> Vec<String> {
        let mut names: BTreeSet<String> = BTreeSet::new();
        for method in self.methods() {
            for desc in [method.input(), method.output()] {
                names.insert(desc.name().to_string());
            }
        }
        names.into_iter().collect()
    }
}

fn suggestion(text: &str, description: &str) -> Suggestion {
    Suggestion {
        value: text.to_string(),
        description: if description.is_empty() {
            None
        } else {
            Some(description.to_string())
        },
        ..Default::default()
    }
}

/// Filters the suggestions by the given prefix, ignoring the case.
fn filter(suggestions: Vec<Suggestion>, prefix: &str) -> Vec<Suggestion> {
    if prefix.is_empty() {
        return suggestions;
    }
    let lower = prefix.to_lowercase();
    suggestions
        .into_iter()
        .filter(|s| s.value.to_lowercase().starts_with(&lower))
        .collect()
}

impl ReedlineCompleter for Completer {
    fn complete(&mut self, line: &str, pos: usize) -> CompletionResult {
        let suggestions = self.suggestions(line, pos);
        CompletionResult::fresh(suggestions)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn completer() -> Completer {
        let fds = protox::compile(["testdata/proto/test.proto"], ["testdata/proto"]).unwrap();
        let pool = prost_reflect::DescriptorPool::from_file_descriptor_set(fds).unwrap();
        Completer::new(Idl::from_pool(pool))
    }

    #[test]
    fn completes_command_names() {
        let completer = completer();
        let suggestions = completer.suggestions("ca", 2);
        assert_eq!(suggestions[0].value, "call");
    }

    #[test]
    fn completes_show_targets() {
        let completer = completer();
        let suggestions = completer.suggestions("show ", 5);
        let values: Vec<&str> = suggestions.iter().map(|s| s.value.as_str()).collect();
        assert!(values.contains(&"package"));
        assert!(values.contains(&"service"));
    }

    #[test]
    fn completes_packages() {
        let completer = completer();
        let suggestions = completer.suggestions("package ", 8);
        assert!(suggestions.iter().any(|s| s.value == "test"));
    }

    #[test]
    fn completes_flags() {
        let completer = completer();
        let suggestions = completer.suggestions("call --en", 9);
        assert!(suggestions.iter().any(|s| s.value == "--enrich"));
    }

    #[test]
    fn completes_methods() {
        let completer = completer();
        let suggestions = completer.suggestions("call ", 5);
        assert!(suggestions.iter().any(|s| s.value == "Unary"));
    }
}
