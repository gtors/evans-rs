//! Descriptor formatting that mirrors `usecase/format_descriptor.go` of the original.
//!
//! The original prints the descriptor with `protoprint` in its compact mode, with fully
//! qualified names and sorted elements.

use prost_reflect::{
    DescriptorPool, EnumDescriptor, FieldDescriptor, Kind, MessageDescriptor, MethodDescriptor,
    OneofDescriptor, ServiceDescriptor, Syntax,
};

/// A symbol that can be described.
pub enum Symbol {
    Message(MessageDescriptor),
    Enum(EnumDescriptor),
    Service(ServiceDescriptor),
    Method(MethodDescriptor),
}

/// Formats the descriptor of a symbol.
pub fn format_descriptor(pool: &DescriptorPool, symbol: &str) -> Option<String> {
    let body = match resolve(pool, symbol)? {
        Symbol::Message(desc) => format_message(&desc),
        Symbol::Enum(desc) => format_enum(&desc),
        Symbol::Service(desc) => format_service(&desc),
        Symbol::Method(desc) => format_method(&desc),
    };
    Some(format!("{symbol}:\n{body}"))
}

/// Resolves a fully-qualified symbol name.
fn resolve(pool: &DescriptorPool, symbol: &str) -> Option<Symbol> {
    if let Some(desc) = pool.get_message_by_name(symbol) {
        return Some(Symbol::Message(desc));
    }
    if let Some(desc) = pool.get_enum_by_name(symbol) {
        return Some(Symbol::Enum(desc));
    }
    if let Some(desc) = pool.get_service_by_name(symbol) {
        return Some(Symbol::Service(desc));
    }
    for service in pool.services() {
        for method in service.methods() {
            if method.full_name() == symbol {
                return Some(Symbol::Method(method));
            }
        }
    }
    None
}

fn format_message(desc: &MessageDescriptor) -> String {
    let mut out = format!("message {} {{", desc.name());

    // The regular fields come first, sorted by their number, and the oneof groups come
    // after them, like `protoprint` prints them.
    let mut fields: Vec<FieldDescriptor> = desc
        .fields()
        .filter(|f| {
            !f.containing_oneof()
                .is_some_and(|oneof| !oneof.is_synthetic())
        })
        .collect();
    fields.sort_by_key(|f| f.number());
    for field in fields {
        out.push_str(&format!("\n  {}", format_field(&field)));
    }

    let mut oneofs: Vec<OneofDescriptor> = desc
        .oneofs()
        .filter(|oneof| !oneof.is_synthetic())
        .collect();
    oneofs.sort_by_key(|oneof| oneof.fields().map(|f| f.number()).min().unwrap_or(0));
    for oneof in oneofs {
        out.push_str(&format_oneof(&oneof));
    }

    out.push_str("\n}");
    out
}

/// Formats the members of a oneof.
fn format_oneof(oneof: &OneofDescriptor) -> String {
    let mut out = format!("\n  oneof {} {{", oneof.name());
    for field in oneof.fields() {
        out.push_str(&format!("\n    {}", format_field(&field)));
    }
    out.push_str("\n  }");
    out
}

fn format_field(field: &FieldDescriptor) -> String {
    let label = if field.is_map() {
        // A map field is rendered as `map<K, V>`, without a label.
        ""
    } else {
        match field.cardinality() {
            prost_reflect::Cardinality::Repeated => "repeated ",
            prost_reflect::Cardinality::Required => "required ",
            prost_reflect::Cardinality::Optional => {
                // The label is printed only if the field is explicitly declared as
                // optional: in proto2 every singular field is, and in proto3 only the
                // fields with the `optional` keyword are. The members of a real oneof
                // have no label.
                let in_oneof = field
                    .containing_oneof()
                    .is_some_and(|oneof| !oneof.is_synthetic());
                let proto3_optional = field
                    .containing_oneof()
                    .is_some_and(|oneof| oneof.is_synthetic());
                let proto2 = field.parent_file().syntax() == Syntax::Proto2;
                if !in_oneof && (proto3_optional || proto2) {
                    "optional "
                } else {
                    ""
                }
            }
        }
    };
    format!(
        "{label}{} {} = {};",
        field_type(field),
        field.name(),
        field.number()
    )
}

fn field_type(field: &FieldDescriptor) -> String {
    if field.is_map() {
        let desc = match field.kind() {
            Kind::Message(desc) => desc,
            _ => return String::new(),
        };
        let key = desc.map_entry_key_field();
        let value = desc.map_entry_value_field();
        return format!("map<{}, {}>", field_type(&key), field_type(&value));
    }

    match field.kind() {
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
        Kind::Message(desc) => format!(".{}", desc.full_name()),
        Kind::Enum(desc) => format!(".{}", desc.full_name()),
    }
}

fn format_enum(desc: &EnumDescriptor) -> String {
    let mut out = format!("enum {} {{", desc.name());
    for value in desc.values() {
        out.push_str(&format!("\n  {} = {};", value.name(), value.number()));
    }
    out.push_str("\n}");
    out
}

fn format_service(desc: &ServiceDescriptor) -> String {
    let mut out = format!("service {} {{", desc.name());
    for method in desc.methods() {
        out.push_str(&format!("\n  {}", format_method(&method)));
    }
    out.push_str("\n}");
    out
}

fn format_method(method: &MethodDescriptor) -> String {
    let input = method.input();
    let output = method.output();
    let stream = |desc: &MessageDescriptor, streaming: bool| {
        if streaming {
            format!("stream .{}", desc.full_name())
        } else {
            format!(".{}", desc.full_name())
        }
    };
    format!(
        "rpc {} ( {} ) returns ( {} );",
        method.name(),
        stream(&input, method.is_client_streaming()),
        stream(&output, method.is_server_streaming())
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pool() -> DescriptorPool {
        let fds = protox::compile(["testdata/proto/test.proto"], ["testdata/proto"]).unwrap();
        DescriptorPool::from_file_descriptor_set(fds).unwrap()
    }

    #[test]
    fn formats_a_message_descriptor() {
        let out = format_descriptor(&pool(), "test.UnaryRequest").unwrap();
        assert!(
            out.starts_with("test.UnaryRequest:\nmessage UnaryRequest {"),
            "{out}"
        );
        assert!(out.contains("\n  string name = 1;"), "{out}");
        assert!(out.contains("\n  repeated string tags = 2;"), "{out}");
        assert!(out.contains("\n  optional int32 age = 4;"), "{out}");
    }

    #[test]
    fn formats_a_service_descriptor() {
        let out = format_descriptor(&pool(), "test.APIService").unwrap();
        assert!(
            out.starts_with("test.APIService:\nservice APIService {"),
            "{out}"
        );
        assert!(
            out.contains("rpc Unary ( .test.UnaryRequest ) returns ( .test.UnaryResponse );"),
            "{out}"
        );
        assert!(
            out.contains(
                "rpc ClientStream ( stream .test.UnaryRequest ) returns ( .test.UnaryResponse );"
            ),
            "{out}"
        );
    }

    #[test]
    fn formats_a_method_descriptor() {
        let out = format_descriptor(&pool(), "test.APIService.Unary").unwrap();
        assert_eq!(
            out,
            "test.APIService.Unary:\nrpc Unary ( .test.UnaryRequest ) returns ( .test.UnaryResponse );"
        );
    }

    #[test]
    fn returns_none_for_an_unknown_symbol() {
        assert!(format_descriptor(&pool(), "test.Nope").is_none());
    }
}
