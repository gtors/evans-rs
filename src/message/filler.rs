//! Interactive filling of dynamic messages.
//!
//! This mirrors `fill/proto/interactive_filler.go` of the original: the user is prompted
//! for each field of the message, nested messages are dug into recursively, enums are
//! selected from a list, repeated fields are filled until the user finishes them and
//! oneof fields let the user pick the member to set.

use std::collections::BTreeSet;

use base64::Engine;
use base64::engine::general_purpose::STANDARD as BASE64;
use prost_reflect::{
    Cardinality, DynamicMessage, FieldDescriptor, Kind, MessageDescriptor, OneofDescriptor,
    ReflectMessage, Value,
};
use thiserror::Error;

/// Errors that can occur while filling a message.
#[derive(Debug, Error)]
pub enum FillError {
    /// The current field is skipped.
    #[error("skipped")]
    Skip,
    /// The rest of the fields are skipped (ctrl+c).
    #[error("aborted")]
    Abort,
    /// The input is over (ctrl+d).
    #[error("end of input")]
    Eof,
    /// The input could not be parsed.
    #[error("invalid value: {0}")]
    Invalid(String),
    /// A file could not be read.
    #[error("failed to read file: {0}")]
    Io(String),
}

/// Prompts the user for values.
pub trait Prompter {
    /// Reads a line of input. `Err(FillError::Abort)` means the user wants to skip the
    /// rest of the fields and `Err(FillError::Eof)` means the input is over.
    fn input(&mut self, prefix: &str) -> Result<String, FillError>;

    /// Reads the path of a file.
    ///
    /// The implementation may complete the paths of the files while they are typed.
    /// The default implementation reads the path like any other input.
    fn input_path(&mut self, prefix: &str) -> Result<String, FillError> {
        self.input(prefix)
    }

    /// Asks the user to choose one of `options` and returns the index of the choice.
    fn select(&mut self, message: &str, options: &[String]) -> Result<usize, FillError>;

    /// Asks the user to choose one of `options` and returns the index of the choice.
    /// `Ok(None)` means that the user canceled the selection.
    fn select_opt(
        &mut self,
        message: &str,
        options: &[String],
    ) -> Result<Option<usize>, FillError> {
        self.select(message, options).map(Some)
    }
}

/// Options that control how fields are filled.
#[derive(Debug, Clone, Copy, Default)]
pub struct FillOptions {
    /// Ask whether to dig down into message fields instead of doing it automatically.
    pub dig_manually: bool,
    /// Ask whether to add another value when a repeated field is encountered.
    pub add_repeated_manually: bool,
}

/// Fills dynamic messages interactively.
pub struct Filler<'a, P: Prompter + ?Sized> {
    prompter: &'a mut P,
    options: FillOptions,
    prefix_format: String,
}

impl<'a, P: Prompter + ?Sized> Filler<'a, P> {
    /// Creates a filler that uses `prompter` to ask the user for values.
    pub fn new(
        prompter: &'a mut P,
        options: FillOptions,
        prefix_format: impl Into<String>,
    ) -> Self {
        Self {
            prompter,
            options,
            prefix_format: prefix_format.into(),
        }
    }

    /// Fills a message of the given type.
    pub fn fill(&mut self, desc: &MessageDescriptor) -> Result<DynamicMessage, FillError> {
        let mut msg = DynamicMessage::new(desc.clone());
        self.resolve(&mut msg, &[], false)?;
        Ok(msg)
    }

    fn resolve(
        &mut self,
        msg: &mut DynamicMessage,
        ancestors: &[String],
        repeated: bool,
    ) -> Result<(), FillError> {
        let desc = msg.descriptor().clone();
        let mut selected_oneofs: BTreeSet<String> = BTreeSet::new();

        for field in desc.fields() {
            // Proto3 optional fields belong to a synthetic oneof. They are filled like
            // ordinary fields.
            let oneof = match field.containing_oneof() {
                Some(oneof) if !oneof.is_synthetic() => Some(oneof),
                _ => None,
            };

            let selected = match oneof {
                Some(oneof) => {
                    if !selected_oneofs.insert(oneof.name().to_string()) {
                        continue;
                    }
                    self.select_oneof(&oneof)?
                }
                None => field.clone(),
            };

            match self.resolve_field(msg, &selected, ancestors, repeated) {
                Ok(()) => {}
                // A skipped field is left unset and the next one is filled.
                Err(FillError::Skip) => continue,
                // ctrl+c stops the filling of the rest of the fields of this message.
                Err(FillError::Abort) => break,
                Err(e) => return Err(e),
            }
        }

        Ok(())
    }

    fn select_oneof(&mut self, oneof: &OneofDescriptor) -> Result<FieldDescriptor, FillError> {
        let options: Vec<String> = oneof.fields().map(|f| f.name().to_string()).collect();
        // Like the original, an aborted selection falls back to the default value.
        let idx = match self.prompter.select(oneof.full_name(), &options) {
            Ok(idx) => idx,
            Err(FillError::Abort) => 0,
            Err(e) => return Err(e),
        };
        oneof.fields().nth(idx).ok_or_else(|| {
            FillError::Invalid(format!(
                "invalid selection: {idx} is out of range for the oneof '{}'",
                oneof.full_name()
            ))
        })
    }

    fn resolve_field(
        &mut self,
        msg: &mut DynamicMessage,
        field: &FieldDescriptor,
        ancestors: &[String],
        repeated: bool,
    ) -> Result<(), FillError> {
        if field.cardinality() == Cardinality::Repeated {
            if field.is_map() {
                return self.resolve_map(msg, field, ancestors, repeated);
            }
            return self.resolve_repeated(msg, field, ancestors, repeated);
        }

        let value = self.resolve_value(field, ancestors, repeated)?;
        msg.set_field(field, value);
        Ok(())
    }

    /// Fills a repeated field. The values are filled until the user finishes the input.
    fn resolve_repeated(
        &mut self,
        msg: &mut DynamicMessage,
        field: &FieldDescriptor,
        ancestors: &[String],
        repeated: bool,
    ) -> Result<(), FillError> {
        loop {
            if !self.add_repeated_value(field)? {
                break;
            }

            let value = match self.resolve_value(field, ancestors, repeated) {
                Ok(v) => v,
                // ctrl+d finishes the repeated field, keeping the inputted values.
                Err(FillError::Eof) => break,
                Err(e) => return Err(e),
            };

            append(msg, field, value);
        }

        Ok(())
    }

    /// Fills a map field. Each entry is filled like a message with a `key` and a `value`
    /// field, like the original does.
    fn resolve_map(
        &mut self,
        msg: &mut DynamicMessage,
        field: &FieldDescriptor,
        ancestors: &[String],
        repeated: bool,
    ) -> Result<(), FillError> {
        let entry_desc = match field.kind() {
            Kind::Message(desc) => desc,
            _ => {
                return Err(FillError::Invalid(format!(
                    "field {} is not a map",
                    field.full_name()
                )));
            }
        };
        let key_desc = entry_desc.map_entry_key_field();
        let value_desc = entry_desc.map_entry_value_field();

        if self.skip_message(field)? {
            return Err(FillError::Skip);
        }

        let mut ancestors = ancestors.to_vec();
        ancestors.push(field.name().to_string());

        loop {
            if !self.add_repeated_value(field)? {
                break;
            }

            let mut entry = DynamicMessage::new(entry_desc.clone());
            match self.resolve(&mut entry, &ancestors, repeated) {
                Ok(()) => {}
                // ctrl+d finishes the map field, keeping the inputted entries.
                Err(FillError::Eof) => break,
                Err(e) => return Err(e),
            }

            let key = entry
                .get_field(&key_desc)
                .into_owned()
                .into_map_key()
                .ok_or_else(|| {
                    FillError::Invalid(format!("invalid map key for field {}", field.full_name()))
                })?;
            let value = entry.get_field(&value_desc).into_owned();
            if let Value::Map(map) = msg.get_field_mut(field) {
                map.insert(key, value);
            }
        }

        Ok(())
    }

    /// Asks whether another value should be added to the repeated field.
    ///
    /// Like the original, the question is asked only when the manual mode is enabled or
    /// when the field is a repeated empty message, which would loop forever otherwise.
    fn add_repeated_value(&mut self, field: &FieldDescriptor) -> Result<bool, FillError> {
        if !self.options.add_repeated_manually {
            let is_empty_message = match field.kind() {
                Kind::Message(desc) => desc.fields().len() == 0,
                _ => false,
            };
            if !is_empty_message {
                return Ok(true);
            }
        }

        let msg = format!("add a repeated field value? field={}", field.full_name());
        let options = vec!["yes".to_string(), "no".to_string()];
        // Like the original, an aborted selection stops adding values.
        let idx = match self.prompter.select(&msg, &options) {
            Ok(idx) => idx,
            Err(_) => return Ok(false),
        };
        Ok(idx == 0)
    }

    fn resolve_value(
        &mut self,
        field: &FieldDescriptor,
        ancestors: &[String],
        repeated: bool,
    ) -> Result<Value, FillError> {
        let prefix = self.make_prefix(field, ancestors, repeated);

        match field.kind() {
            Kind::Message(desc) => {
                if self.skip_message(field)? {
                    return Err(FillError::Skip);
                }

                let mut ancestors = ancestors.to_vec();
                ancestors.push(field.name().to_string());
                let mut nested = DynamicMessage::new(desc.clone());
                self.resolve(&mut nested, &ancestors, repeated || field.is_list())?;
                Ok(Value::Message(nested))
            }
            Kind::Enum(desc) => {
                let options: Vec<String> = desc.values().map(|v| v.name().to_string()).collect();
                // Like the original, an aborted selection falls back to the default value.
                let idx = match self.prompter.select(&prefix, &options) {
                    Ok(idx) => idx,
                    Err(FillError::Abort) => 0,
                    Err(e) => return Err(e),
                };
                let number = desc
                    .values()
                    .nth(idx)
                    .map(|v| v.number())
                    .unwrap_or_default();
                Ok(Value::EnumNumber(number))
            }
            Kind::Bytes => Ok(Value::Bytes(self.resolve_bytes(&prefix)?)),
            _ => {
                let input = self.prompter.input(&prefix)?;
                if input.is_empty() {
                    return Ok(default_value(field));
                }
                convert(&field.kind(), &input)
            }
        }
    }

    /// Asks the user how the bytes are provided and reads them.
    ///
    /// The bytes are either a base64-encoded string or the contents of a file, whose
    /// path is completed while it is typed.
    fn resolve_bytes(&mut self, prefix: &str) -> Result<prost_reflect::bytes::Bytes, FillError> {
        let options = vec!["base64".to_string(), "file".to_string()];
        // Like the original, an aborted selection falls back to the default value.
        let idx = match self.prompter.select(prefix, &options) {
            Ok(idx) => idx,
            Err(FillError::Abort) => 0,
            Err(e) => return Err(e),
        };

        match idx {
            0 => {
                let input = self.prompter.input(prefix)?;
                if input.is_empty() {
                    return Ok(prost_reflect::bytes::Bytes::new());
                }
                decode_base64(&input)
                    .map(prost_reflect::bytes::Bytes::from)
                    .map_err(FillError::Invalid)
            }
            1 => {
                let path = self.prompter.input_path(prefix)?;
                let contents =
                    std::fs::read(&path).map_err(|e| FillError::Io(format!("{path}: {e}")))?;
                Ok(prost_reflect::bytes::Bytes::from(contents))
            }
            _ => Err(FillError::Invalid(format!(
                "invalid selection: {idx} is out of range for the bytes"
            ))),
        }
    }

    /// Asks whether the user wants to dig down into a message field. It returns true if
    /// the field should be skipped.
    fn skip_message(&mut self, field: &FieldDescriptor) -> Result<bool, FillError> {
        if !self.options.dig_manually {
            return Ok(false);
        }

        let msg = format!("dig down? field={}", field.full_name());
        let options = vec!["dig down".to_string(), "skip".to_string()];
        // Like the original, an aborted selection digs down anyway.
        let idx = self.prompter.select(&msg, &options).unwrap_or(0);
        Ok(idx == 1)
    }

    fn make_prefix(&self, field: &FieldDescriptor, ancestors: &[String], repeated: bool) -> String {
        let delimiter = "::";
        let mut joined = ancestors.join(delimiter);
        if !joined.is_empty() {
            joined.push_str(delimiter);
        }

        let mut prefix = self
            .prefix_format
            .replace("{ancestor}", &joined)
            .replace("{name}", field.name())
            .replace("{type}", kind_name(&field.kind()));

        if repeated || field.is_list() {
            prefix = format!("<repeated> {prefix}");
        }

        prefix
    }
}

/// Appends a value to a repeated field of the message.
fn append(msg: &mut DynamicMessage, field: &FieldDescriptor, value: Value) {
    let current = msg.get_field_mut(field);
    if let Value::List(list) = current {
        list.push(value);
    }
}

/// The default value of a field. Repeated fields use the default value of their element
/// kind, like the original does.
fn default_value(field: &FieldDescriptor) -> Value {
    if field.cardinality() == Cardinality::Repeated {
        field.kind().default_value()
    } else {
        field.default_value()
    }
}

/// The name of a kind as it is displayed to the user, mirroring `protoreflect.Kind`.
fn kind_name(kind: &Kind) -> &'static str {
    match kind {
        Kind::Double => "double",
        Kind::Float => "float",
        Kind::Int64 => "int64",
        Kind::Uint64 => "uint64",
        Kind::Int32 => "int32",
        Kind::Uint32 => "uint32",
        Kind::Sint32 => "sint32",
        Kind::Sint64 => "sint64",
        Kind::Fixed32 => "fixed32",
        Kind::Fixed64 => "fixed64",
        Kind::Sfixed32 => "sfixed32",
        Kind::Sfixed64 => "sfixed64",
        Kind::Bool => "bool",
        Kind::String => "string",
        Kind::Bytes => "bytes",
        Kind::Message(_) => "message",
        Kind::Enum(_) => "enum",
    }
}

fn convert(kind: &Kind, input: &str) -> Result<Value, FillError> {
    let invalid = |input: &str| FillError::Invalid(input.to_string());

    Ok(match kind {
        Kind::Double => Value::F64(input.parse::<f64>().map_err(|_| invalid(input))?),
        Kind::Float => Value::F32(input.parse::<f32>().map_err(|_| invalid(input))?),
        Kind::Int64 | Kind::Sfixed64 | Kind::Sint64 => {
            Value::I64(input.parse::<i64>().map_err(|_| invalid(input))?)
        }
        Kind::Uint64 | Kind::Fixed64 => {
            Value::U64(input.parse::<u64>().map_err(|_| invalid(input))?)
        }
        Kind::Int32 | Kind::Sfixed32 | Kind::Sint32 => {
            Value::I32(input.parse::<i32>().map_err(|_| invalid(input))?)
        }
        Kind::Uint32 | Kind::Fixed32 => {
            Value::U32(input.parse::<u32>().map_err(|_| invalid(input))?)
        }
        Kind::Bool => Value::Bool(parse_bool(input).ok_or_else(|| invalid(input))?),
        Kind::String => Value::String(input.to_string()),
        Kind::Message(_) | Kind::Enum(_) | Kind::Bytes => {
            return Err(FillError::Invalid(format!(
                "the type {kind:?} cannot be converted from a string"
            )));
        }
    })
}

fn parse_bool(input: &str) -> Option<bool> {
    match input {
        "1" | "t" | "T" | "TRUE" | "true" | "True" => Some(true),
        "0" | "f" | "F" | "FALSE" | "false" | "False" => Some(false),
        _ => None,
    }
}

/// Decodes a standard base64 string. The padding is required and the trailing bits must
/// be zero, like `base64.StdEncoding` of the original does.
fn decode_base64(input: &str) -> Result<Vec<u8>, String> {
    BASE64
        .decode(input)
        .map_err(|e| format!("invalid base64 input: {input}: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    use prost_reflect::{DescriptorPool, MapKey};

    struct StubPrompter {
        inputs: Vec<String>,
        selections: Vec<usize>,
        idx: usize,
        prefixes: Vec<String>,
    }

    impl StubPrompter {
        fn new(inputs: &[&str], selections: &[usize]) -> Self {
            Self {
                inputs: inputs.iter().map(|s| s.to_string()).collect(),
                selections: selections.to_vec(),
                idx: 0,
                prefixes: Vec::new(),
            }
        }
    }

    impl Prompter for StubPrompter {
        fn input(&mut self, prefix: &str) -> Result<String, FillError> {
            self.prefixes.push(prefix.to_string());
            if self.inputs.is_empty() {
                return Err(FillError::Eof);
            }
            Ok(self.inputs.remove(0))
        }

        fn select(&mut self, _message: &str, _options: &[String]) -> Result<usize, FillError> {
            let sel = *self
                .selections
                .get(self.idx)
                .expect("selection index is in range");
            self.idx += 1;
            Ok(sel)
        }
    }

    fn pool() -> DescriptorPool {
        let fds = protox::compile(["testdata/proto/test.proto"], ["testdata/proto"]).unwrap();
        DescriptorPool::from_file_descriptor_set(fds).unwrap()
    }

    fn fill(
        message: &str,
        inputs: &[&str],
        selections: &[usize],
        options: FillOptions,
    ) -> Result<DynamicMessage, FillError> {
        fill_with_prefixes(message, inputs, selections, options).0
    }

    /// Fills a message and returns the prefixes of the prompts as well.
    fn fill_with_prefixes(
        message: &str,
        inputs: &[&str],
        selections: &[usize],
        options: FillOptions,
    ) -> (Result<DynamicMessage, FillError>, Vec<String>) {
        let pool = pool();
        let desc = pool
            .get_message_by_name(message)
            .unwrap_or_else(|| panic!("{message} should exist"));
        let mut prompter = StubPrompter::new(inputs, selections);
        let mut filler = Filler::new(&mut prompter, options, "{ancestor}{name} ({type}) => ");
        let result = filler.fill(&desc);
        (result, prompter.prefixes)
    }

    #[test]
    fn fills_scalars() {
        let msg = fill(
            "test.FillScalars",
            &["ktr", "20"],
            &[],
            FillOptions::default(),
        )
        .unwrap();
        assert_eq!(msg.get_field_by_name("name").unwrap().as_str(), Some("ktr"));
        assert_eq!(msg.get_field_by_name("age").unwrap().as_i32(), Some(20));
    }

    #[test]
    fn fills_enum_by_selection() {
        let msg = fill("test.FillEnum", &[], &[1], FillOptions::default()).unwrap();
        assert_eq!(
            msg.get_field_by_name("suit").unwrap().as_enum_number(),
            Some(1)
        );
    }

    #[test]
    fn empty_input_uses_the_default_value() {
        let msg = fill("test.FillScalars", &["", ""], &[], FillOptions::default()).unwrap();
        assert_eq!(msg.get_field_by_name("name").unwrap().as_str(), Some(""));
    }

    #[test]
    fn fills_nested_messages() {
        let msg = fill(
            "test.FillNestedRequest",
            &["1"],
            &[],
            FillOptions::default(),
        )
        .unwrap();
        let nested = msg.get_field_by_name("nested").unwrap();
        let nested = nested.as_message().unwrap();
        assert_eq!(nested.get_field_by_name("id").unwrap().as_str(), Some("1"));
    }

    #[test]
    fn dig_manually_skips_nested_messages() {
        let msg = fill(
            "test.FillNestedRequest",
            &[],
            &[1],
            FillOptions {
                dig_manually: true,
                ..Default::default()
            },
        )
        .unwrap();
        assert!(!msg.has_field_by_name("nested"));
    }

    #[test]
    fn fills_repeated_fields_until_eof() {
        let msg = fill(
            "test.FillRepeated",
            &["a", "b"],
            &[],
            FillOptions::default(),
        )
        .unwrap();
        let tags = msg.get_field_by_name("tags").unwrap();
        let list = tags.as_list().unwrap();
        assert_eq!(list.len(), 2);
        assert_eq!(list[1].as_str(), Some("b"));
    }

    #[test]
    fn add_repeated_manually_stops_on_no() {
        let msg = fill(
            "test.FillRepeated",
            &["a"],
            &[0, 1],
            FillOptions {
                add_repeated_manually: true,
                ..Default::default()
            },
        )
        .unwrap();
        let tags = msg.get_field_by_name("tags").unwrap();
        assert_eq!(tags.as_list().unwrap().len(), 1);
    }

    #[test]
    fn decodes_bytes_from_base64() {
        // The first selection chooses the format of the bytes.
        let msg = fill(
            "test.FillBytes",
            &["SGVsbG8="],
            &[0],
            FillOptions::default(),
        )
        .unwrap();
        let data = msg.get_field_by_name("payload").unwrap();
        assert_eq!(data.as_bytes().unwrap().as_ref(), b"Hello");
    }

    #[test]
    fn reads_bytes_from_a_file() {
        let path = std::env::temp_dir().join(format!("evans-bytes-{}.bin", std::process::id()));
        std::fs::write(&path, b"\x00\x01\x02binary").unwrap();

        // The second selection chooses the file.
        let msg = fill(
            "test.FillBytes",
            &[path.to_str().unwrap()],
            &[1],
            FillOptions::default(),
        )
        .unwrap();
        let data = msg.get_field_by_name("payload").unwrap();
        assert_eq!(data.as_bytes().unwrap().as_ref(), b"\x00\x01\x02binary");

        std::fs::remove_file(&path).unwrap();
    }

    #[test]
    fn reports_a_missing_file_of_the_bytes() {
        let err = fill(
            "test.FillBytes",
            &["/nonexistent/file.bin"],
            &[1],
            FillOptions::default(),
        )
        .unwrap_err();
        assert!(err.to_string().contains("/nonexistent/file.bin"), "{err}");
    }

    #[test]
    fn selects_oneof_members() {
        let msg = fill("test.FillOneof", &["ktr"], &[0], FillOptions::default()).unwrap();
        assert_eq!(msg.get_field_by_name("text").unwrap().as_str(), Some("ktr"));
        assert!(!msg.has_field_by_name("raw"));
    }

    #[test]
    fn fills_map_fields() {
        let msg = fill(
            "test.FillMap",
            &["key", "value", "key2", "value2"],
            &[],
            FillOptions::default(),
        )
        .unwrap();
        let attrs = msg.get_field_by_name("attrs").unwrap();
        let map = attrs.as_map().unwrap();
        assert_eq!(map.len(), 2);
        assert_eq!(
            map.get(&MapKey::String("key".to_string()))
                .unwrap()
                .as_str(),
            Some("value")
        );
        assert_eq!(
            map.get(&MapKey::String("key2".to_string()))
                .unwrap()
                .as_str(),
            Some("value2")
        );
    }

    #[test]
    fn marks_the_fields_of_a_repeated_message_as_repeated() {
        let (_, prefixes) = fill_with_prefixes(
            "test.FillRepeatedMessage",
            &["1", "2"],
            &[],
            FillOptions::default(),
        );
        assert_eq!(
            prefixes,
            vec![
                "<repeated> items::id (string) => ",
                "<repeated> items::id (string) => ",
                "<repeated> items::id (string) => ",
            ]
        );
        assert!(prefixes.iter().all(|p| p.starts_with("<repeated> ")));
    }

    #[test]
    fn ctrl_c_skips_the_rest_of_the_fields_of_the_message() {
        let mut prompter = AbortingPrompter::new();
        let pool = pool();
        let desc = pool.get_message_by_name("test.FillScalars").unwrap();
        let mut filler = Filler::new(&mut prompter, FillOptions::default(), "{name} => ");
        let msg = filler.fill(&desc).unwrap();
        // Both fields are skipped: the first one is aborted and the second one is never
        // prompted.
        assert_eq!(prompter.prompts, 1);
        assert!(!msg.has_field_by_name("name"));
        assert!(!msg.has_field_by_name("age"));
    }

    #[test]
    fn rejects_invalid_base64_input() {
        let err = fill("test.FillBytes", &["SGVsbG8"], &[0], FillOptions::default()).unwrap_err();
        assert!(matches!(err, FillError::Invalid(_)), "{err:?}");
    }

    #[test]
    fn decodes_base64_like_the_original() {
        // The expectations are taken from `base64.StdEncoding` of the original.
        assert_eq!(decode_base64("SGVsbG8=").unwrap(), b"Hello");
        assert!(decode_base64("SGVsbG8").is_err());
        assert!(decode_base64("hello").is_err());
        assert!(decode_base64("SGVsbG8=!!!").is_err());
        assert!(decode_base64("").unwrap().is_empty());
    }

    /// A prompter that aborts every input.
    struct AbortingPrompter {
        prompts: usize,
    }

    impl AbortingPrompter {
        fn new() -> Self {
            Self { prompts: 0 }
        }
    }

    impl Prompter for AbortingPrompter {
        fn input(&mut self, _prefix: &str) -> Result<String, FillError> {
            self.prompts += 1;
            Err(FillError::Abort)
        }

        fn select(&mut self, _message: &str, _options: &[String]) -> Result<usize, FillError> {
            Ok(0)
        }
    }
}
