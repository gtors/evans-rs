//! The completer of the REPL, mirroring `repl/completer.go` of the original.

use std::collections::BTreeMap;
use std::sync::{Arc, RwLock};

use reedline::{Completer as ReedlineCompleter, CompletionResult, Span, Suggestion};

use crate::idl::Idl;

use super::command;

/// The context of the completion.
///
/// It mirrors the state of the REPL, which changes while the session is running, so
/// that the suggestions follow the currently selected service and the headers that
/// are already set.
#[derive(Debug, Default, Clone)]
pub struct CompletionContext {
    /// The fully-qualified name of the currently selected service.
    pub service: String,
    /// The keys of the headers that are currently set.
    pub header_keys: Vec<String>,
}

/// The completer of the REPL.
pub struct Completer {
    idl: Idl,
    context: Arc<RwLock<CompletionContext>>,
}

impl Completer {
    /// Creates a new completer.
    pub fn new(idl: Idl, context: Arc<RwLock<CompletionContext>>) -> Self {
        Self { idl, context }
    }

    /// Returns the suggestions for the given input.
    pub fn suggestions(&self, line: &str, pos: usize) -> Vec<Suggestion> {
        let pos = pos.min(line.len());
        let Some(before) = line.get(..pos) else {
            return Vec::new();
        };

        // The word before the cursor is the text that a suggestion replaces.
        let word_start = before
            .rfind(char::is_whitespace)
            .map(|i| i + 1)
            .unwrap_or(0);
        let word = &before[word_start..];

        let args: Vec<&str> = before.split_whitespace().collect();
        let cmd_name = args.first().copied().unwrap_or("");
        // The flags are not positional arguments, so they are not counted.
        let positional = args
            .iter()
            .skip(1)
            .filter(|arg| !arg.starts_with('-'))
            .count();

        let lower = word.to_lowercase();
        self.candidates(cmd_name, positional, word)
            .into_iter()
            .filter(|candidate| candidate.matches(&lower))
            .map(|candidate| Suggestion {
                value: candidate.value,
                description: candidate.description,
                span: Span::new(word_start, pos),
                ..Default::default()
            })
            .collect()
    }

    /// Returns the candidates of the completion.
    fn candidates(&self, cmd_name: &str, positional: usize, word: &str) -> Vec<Candidate> {
        let Some(cmd) = command::lookup(cmd_name) else {
            // The first word of the line is the name of a command.
            if positional > 0 {
                return Vec::new();
            }
            let mut candidates = vec![Candidate::new("help", "show help message")];
            for cmd in command::COMMANDS {
                candidates.push(Candidate::new(cmd.name, cmd.synopsis));
            }
            for (alias, _) in command::ALIASES {
                candidates.push(Candidate::new(*alias, "alias of a command"));
            }
            return candidates;
        };

        // A flag is suggested when the word starts with a dash.
        if word.starts_with('-') {
            let mut candidates: Vec<Candidate> = Vec::new();
            for flag in cmd.flags {
                candidates.push(Candidate::new(format!("--{}", flag.name), flag.usage));
                if let Some(short) = flag.short {
                    candidates.push(Candidate::new(format!("-{short}"), flag.usage));
                }
            }
            candidates.push(Candidate::new("--help", "show command help message"));
            return candidates;
        }

        // Only the first positional argument is completed.
        if positional > 1 {
            return Vec::new();
        }

        match cmd.name {
            "show" => vec![
                Candidate::new("package", "show loaded package names"),
                Candidate::new("service", "show loaded service names"),
                Candidate::new("message", "show loaded message names"),
                Candidate::new(
                    "rpc",
                    "show RPC names belonging to the current selected service",
                ),
                Candidate::new("header", "show headers which will be added to each request"),
            ],
            "call" => self.method_candidates(),
            "desc" => self.message_candidates(),
            "header" => self.header_candidates(word),
            _ => Vec::new(),
        }
    }

    /// Returns the candidates of the methods of the loaded services.
    ///
    /// The fully-qualified name is suggested, and the short name of the method is
    /// accepted as an alias of it.
    fn method_candidates(&self) -> Vec<Candidate> {
        self.methods()
            .into_iter()
            .map(|method| {
                Candidate::with_alias(
                    method.full_name().to_string(),
                    format!(
                        "{} -> {}",
                        method.input().full_name(),
                        method.output().full_name()
                    ),
                    method.name().to_string(),
                )
            })
            .collect()
    }

    /// Returns the candidates of the messages.
    ///
    /// The fully-qualified name is suggested, and the short name of the message is
    /// accepted as an alias of it.
    fn message_candidates(&self) -> Vec<Candidate> {
        let mut messages: BTreeMap<String, String> = BTreeMap::new();
        for method in self.methods() {
            for desc in [method.input(), method.output()] {
                messages.insert(desc.full_name().to_string(), desc.name().to_string());
            }
        }
        messages
            .into_iter()
            .map(|(fqn, short)| Candidate::with_alias(fqn, "", short))
            .collect()
    }

    /// Returns the candidates of the header keys that are already set.
    fn header_candidates(&self, word: &str) -> Vec<Candidate> {
        // The value of a header is being typed, so the key is not completed.
        if word.contains('=') {
            return Vec::new();
        }

        let Ok(context) = self.context.read() else {
            return Vec::new();
        };
        context
            .header_keys
            .iter()
            .map(|key| Candidate::new(format!("{key}="), format!("set the header {key}")))
            .collect()
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
}

impl ReedlineCompleter for Completer {
    fn complete(&mut self, line: &str, pos: usize) -> CompletionResult {
        let suggestions = self.suggestions(line, pos);
        CompletionResult::fresh(suggestions)
    }
}

/// A candidate of the completion.
struct Candidate {
    /// The text that replaces the word before the cursor.
    value: String,
    /// The description that is shown next to the value.
    description: Option<String>,
    /// The other texts that the word before the cursor may match.
    aliases: Vec<String>,
}

impl Candidate {
    /// Creates a candidate without a description.
    fn new(value: impl Into<String>, description: impl Into<String>) -> Self {
        let description = description.into();
        Self {
            value: value.into(),
            description: if description.is_empty() {
                None
            } else {
                Some(description)
            },
            aliases: Vec::new(),
        }
    }

    /// Creates a candidate that also matches the given alias.
    fn with_alias(value: impl Into<String>, description: impl Into<String>, alias: String) -> Self {
        let mut candidate = Self::new(value, description);
        candidate.aliases.push(alias);
        candidate
    }

    /// Returns whether the candidate matches the given word, which is already
    /// lowercased.
    fn matches(&self, word: &str) -> bool {
        if self.value.to_lowercase().starts_with(word) {
            return true;
        }
        self.aliases
            .iter()
            .any(|alias| alias.to_lowercase().starts_with(word))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn completer() -> Completer {
        completer_with(CompletionContext::default())
    }

    fn completer_with(context: CompletionContext) -> Completer {
        let fds = protox::compile(["testdata/proto/test.proto"], ["testdata/proto"]).unwrap();
        let pool = prost_reflect::DescriptorPool::from_file_descriptor_set(fds).unwrap();
        Completer::new(Idl::from_pool(pool), Arc::new(RwLock::new(context)))
    }

    #[test]
    fn completes_command_names() {
        let completer = completer();
        let suggestions = completer.suggestions("ca", 2);
        assert_eq!(suggestions[0].value, "call");
    }

    #[test]
    fn completes_command_names_of_an_empty_line() {
        let completer = completer();
        let suggestions = completer.suggestions("", 0);
        let values: Vec<&str> = suggestions.iter().map(|s| s.value.as_str()).collect();
        assert!(values.contains(&"help"));
        assert!(values.contains(&"call"));
        assert!(values.contains(&"exit"));
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
    fn does_not_complete_the_package_command() {
        // The package is not a command anymore, so it is not suggested.
        assert!(command::lookup("package").is_none());
        let completer = completer();
        let suggestions = completer.suggestions("pack", 4);
        assert!(suggestions.is_empty());
    }

    #[test]
    fn completes_flags() {
        let completer = completer();
        let suggestions = completer.suggestions("call --en", 9);
        assert!(suggestions.iter().any(|s| s.value == "--enrich"));
    }

    #[test]
    fn completes_the_help_flag() {
        let completer = completer();
        let suggestions = completer.suggestions("call --h", 8);
        assert!(suggestions.iter().any(|s| s.value == "--help"));
    }

    #[test]
    fn completes_methods() {
        let completer = completer();
        let suggestions = completer.suggestions("call ", 5);
        assert!(
            suggestions
                .iter()
                .any(|s| s.value == "test.APIService.Unary")
        );
    }

    #[test]
    fn completes_methods_by_their_short_name() {
        let completer = completer();
        let suggestions = completer.suggestions("call Un", 7);
        assert!(
            suggestions
                .iter()
                .any(|s| s.value == "test.APIService.Unary")
        );
    }

    #[test]
    fn completes_methods_after_the_flags() {
        let completer = completer();
        let suggestions = completer.suggestions("call --enrich --emit-defaults ", 31);
        assert!(
            suggestions
                .iter()
                .any(|s| s.value == "test.APIService.Unary")
        );
    }

    #[test]
    fn completes_messages() {
        let completer = completer();
        let suggestions = completer.suggestions("desc ", 5);
        assert!(suggestions.iter().any(|s| s.value == "test.UnaryRequest"));
    }

    #[test]
    fn completes_the_header_keys() {
        let completer = completer_with(CompletionContext {
            service: "test.APIService".to_string(),
            header_keys: vec!["x-token".to_string()],
        });
        let suggestions = completer.suggestions("header ", 7);
        assert!(suggestions.iter().any(|s| s.value == "x-token="));
    }

    #[test]
    fn the_span_covers_the_word_before_the_cursor() {
        let completer = completer();
        let suggestions = completer.suggestions("call Un", 7);
        let suggestion = suggestions
            .iter()
            .find(|s| s.value == "test.APIService.Unary")
            .unwrap();
        assert_eq!(suggestion.span.start, 5);
        assert_eq!(suggestion.span.end, 7);
    }

    #[test]
    fn does_not_complete_the_service_command() {
        // The service is not a command anymore, so it is not suggested.
        assert!(command::lookup("service").is_none());
        let completer = completer();
        let suggestions = completer.suggestions("serv", 4);
        assert!(suggestions.is_empty());
    }
}
