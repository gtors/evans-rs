pub mod proto;
pub mod reflection;

use std::collections::HashMap;

use prost_reflect::{DescriptorPool, MessageDescriptor, MethodDescriptor, ServiceDescriptor};

pub const REFLECTION_SERVICE_V1: &str = "grpc.reflection.v1.ServerReflection";
pub const REFLECTION_SERVICE_V1ALPHA: &str = "grpc.reflection.v1alpha.ServerReflection";

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("unknown service name '{0}'")]
    UnknownServiceName(String),
    #[error("unknown RPC name")]
    UnknownRpcName,
    #[error("unknown symbol")]
    UnknownSymbol,
    #[error("unknown method name '{0}'")]
    UnknownMethodName(String),
    #[error("unknown message name '{0}'")]
    UnknownMessageName(String),
    #[error("ambiguous {kind} '{name}'. the candidates are: {candidates}")]
    AmbiguousName {
        kind: &'static str,
        name: String,
        candidates: String,
    },
}

pub type Result<T> = std::result::Result<T, Error>;

#[derive(Debug, Clone)]
pub struct Idl {
    pool: DescriptorPool,
    /// The fully-qualified name of every method, with the name of its service.
    methods: HashMap<String, (String, String)>,
}

impl Idl {
    pub fn from_pool(pool: DescriptorPool) -> Self {
        let mut methods: HashMap<String, (String, String)> = HashMap::new();
        for service in pool.services() {
            let fqsn = service.full_name().to_string();
            for method in service.methods() {
                methods.insert(
                    method.full_name().to_string(),
                    (fqsn.clone(), method.name().to_string()),
                );
            }
        }
        Self { pool, methods }
    }

    pub fn pool(&self) -> &DescriptorPool {
        &self.pool
    }

    pub fn list_services(&self) -> Vec<String> {
        let mut names: Vec<String> = self
            .pool
            .services()
            .map(|s| s.full_name().to_string())
            .collect();
        names.sort();
        names
    }

    pub fn list_packages(&self) -> Vec<String> {
        let mut pkgs: Vec<String> = self
            .list_services()
            .into_iter()
            .map(|fqsn| split_fully_qualified_service_name(&fqsn).0.to_string())
            .collect();
        pkgs.sort();
        pkgs.dedup();
        pkgs
    }

    pub fn service(&self, fqsn: &str) -> Result<ServiceDescriptor> {
        self.pool
            .get_service_by_name(fqsn)
            .ok_or_else(|| Error::UnknownServiceName(fqsn.to_string()))
    }

    pub fn methods(&self, fqsn: &str) -> Result<Vec<MethodDescriptor>> {
        Ok(self.service(fqsn)?.methods().collect())
    }

    pub fn method(&self, fqsn: &str, name: &str) -> Result<MethodDescriptor> {
        self.service(fqsn)?
            .methods()
            .find(|m| m.name() == name)
            .ok_or(Error::UnknownRpcName)
    }

    /// Resolves a message by its fully-qualified name or by its short name.
    ///
    /// The short name is ambiguous when several messages share it.
    pub fn resolve_message(&self, name: &str) -> Result<MessageDescriptor> {
        if let Some(desc) = self.pool.get_message_by_name(name) {
            return Ok(desc);
        }

        let candidates: Vec<MessageDescriptor> = self
            .pool
            .all_messages()
            .filter(|message| message.name() == name)
            .collect();

        match candidates.as_slice() {
            [desc] => Ok(desc.clone()),
            [] => Err(Error::UnknownMessageName(name.to_string())),
            many => Err(Error::AmbiguousName {
                kind: "message name",
                name: name.to_string(),
                candidates: join_names(many.iter().map(|m| m.full_name())),
            }),
        }
    }

    pub fn resolve_fully_qualified_method_name(&self, fqmn: &str) -> Result<(String, String)> {
        self.methods.get(fqmn).cloned().ok_or(Error::UnknownSymbol)
    }

    /// Resolves a method by its fully-qualified name, like `pkg.Service.Method`, or by
    /// its short name.
    ///
    /// The short name is ambiguous when several methods share it.
    pub fn resolve_method(&self, name: &str) -> Result<MethodDescriptor> {
        if let Some((fqsn, method)) = self.methods.get(name) {
            return self.method(fqsn, method);
        }

        let candidates: Vec<MethodDescriptor> = self
            .pool
            .services()
            .flat_map(|service| service.methods().collect::<Vec<_>>())
            .filter(|method| method.name() == name)
            .collect();

        match candidates.as_slice() {
            [desc] => Ok(desc.clone()),
            [] => Err(Error::UnknownMethodName(name.to_string())),
            many => Err(Error::AmbiguousName {
                kind: "method name",
                name: name.to_string(),
                candidates: join_names(many.iter().map(|m| m.full_name())),
            }),
        }
    }
}

/// Joins the names of the candidates of an ambiguous symbol.
fn join_names<'a>(names: impl Iterator<Item = &'a str>) -> String {
    names.collect::<Vec<_>>().join(", ")
}

/// Splits a fully-qualified service name into its package and its service name.
pub fn split_fully_qualified_service_name(fqsn: &str) -> (&str, &str) {
    match fqsn.rfind('.') {
        None => ("", fqsn),
        Some(i) => (&fqsn[..i], &fqsn[i + 1..]),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn idl() -> Idl {
        Idl::from_files(&[], &["testdata/proto/test.proto".to_string()])
            .expect("failed to load test proto")
    }

    #[test]
    fn lists_packages_and_services() {
        let idl = idl();
        assert_eq!(idl.list_packages(), vec!["test"]);
        assert_eq!(
            idl.list_services(),
            vec!["test.APIService", "test.EmptyService"]
        );
    }

    #[test]
    fn methods_and_streaming_kinds() {
        let idl = idl();
        let mut names: Vec<String> = idl
            .methods("test.APIService")
            .unwrap()
            .iter()
            .map(|m| m.name().to_string())
            .collect();
        names.sort();
        assert_eq!(
            names,
            vec![
                "BidirectionalStream",
                "ClientStream",
                "ServerStream",
                "Simple",
                "SimpleClientStream",
                "SimpleStream",
                "Unary",
                "UnaryFailure"
            ]
        );

        let unary = idl.method("test.APIService", "Unary").unwrap();
        assert!(!unary.is_client_streaming());
        assert!(!unary.is_server_streaming());
        assert_eq!(unary.input().full_name(), "test.UnaryRequest");

        let cs = idl.method("test.APIService", "ClientStream").unwrap();
        assert!(cs.is_client_streaming());
        assert!(!cs.is_server_streaming());

        let ss = idl.method("test.APIService", "ServerStream").unwrap();
        assert!(!ss.is_client_streaming());
        assert!(ss.is_server_streaming());

        let bs = idl
            .method("test.APIService", "BidirectionalStream")
            .unwrap();
        assert!(bs.is_client_streaming() && bs.is_server_streaming());
    }

    #[test]
    fn parse_fully_qualified_method_name() {
        let idl = idl();
        let (fqsn, mtd) = idl
            .resolve_fully_qualified_method_name("test.APIService.Unary")
            .unwrap();
        assert_eq!(fqsn, "test.APIService");
        assert_eq!(mtd, "Unary");

        assert!(matches!(
            idl.resolve_fully_qualified_method_name("test.APIService.Nope"),
            Err(Error::UnknownSymbol)
        ));
        assert!(matches!(
            idl.resolve_fully_qualified_method_name("test.UnaryRequest"),
            Err(Error::UnknownSymbol)
        ));
        assert!(matches!(
            idl.resolve_fully_qualified_method_name("nodot"),
            Err(Error::UnknownSymbol)
        ));
    }

    #[test]
    fn resolves_a_method_by_its_fully_qualified_name() {
        let idl = idl();
        let method = idl.resolve_method("test.APIService.Unary").unwrap();
        assert_eq!(method.name(), "Unary");
        assert!(matches!(
            idl.resolve_method("test.APIService.Nope"),
            Err(Error::UnknownMethodName(_))
        ));
    }

    #[test]
    fn unknown_service() {
        let idl = idl();
        assert!(matches!(
            idl.service("test.Nope"),
            Err(Error::UnknownServiceName(_))
        ));
        assert!(matches!(
            idl.method("test.APIService", "Nope"),
            Err(Error::UnknownRpcName)
        ));
    }

    #[test]
    fn resolves_a_method_by_its_short_name() {
        let idl = idl();
        // The short name is resolved when it is unique.
        assert_eq!(
            idl.resolve_method("Simple").unwrap().full_name(),
            "test.APIService.Simple"
        );
        assert!(matches!(
            idl.resolve_method("Nope"),
            Err(Error::UnknownMethodName(_))
        ));
    }

    #[test]
    fn resolves_a_message_by_its_short_name() {
        let idl = idl();
        assert_eq!(
            idl.resolve_message("UnaryRequest").unwrap().full_name(),
            "test.UnaryRequest"
        );
        assert!(matches!(
            idl.resolve_message("Nope"),
            Err(Error::UnknownMessageName(_))
        ));
    }

    #[test]
    fn reports_an_ambiguous_name() {
        let fds = protox::compile(
            [
                "testdata/proto/test.proto",
                "testdata/proto/ambiguous.proto",
            ],
            ["testdata/proto"],
        )
        .unwrap();
        let pool = DescriptorPool::from_file_descriptor_set(fds).unwrap();
        let idl = Idl::from_pool(pool);

        match idl.resolve_method("Unary") {
            Err(Error::AmbiguousName {
                kind,
                name,
                candidates,
            }) => {
                assert_eq!(kind, "method name");
                assert_eq!(name, "Unary");
                assert!(candidates.contains("test.APIService.Unary"), "{candidates}");
                assert!(
                    candidates.contains("other.APIService.Unary"),
                    "{candidates}"
                );
            }
            other => panic!("expected an ambiguous method name, but got {other:?}"),
        }

        match idl.resolve_message("UnaryRequest") {
            Err(Error::AmbiguousName {
                kind,
                name,
                candidates,
            }) => {
                assert_eq!(kind, "message name");
                assert_eq!(name, "UnaryRequest");
                assert!(candidates.contains("test.UnaryRequest"), "{candidates}");
                assert!(candidates.contains("other.UnaryRequest"), "{candidates}");
            }
            other => panic!("expected an ambiguous message name, but got {other:?}"),
        }
    }

    #[test]
    fn splits_a_fully_qualified_service_name() {
        assert_eq!(
            split_fully_qualified_service_name("a.b.Service"),
            ("a.b", "Service")
        );
        assert_eq!(
            split_fully_qualified_service_name("Service"),
            ("", "Service")
        );
    }
}
