pub mod proto;
pub mod reflection;

use std::collections::HashMap;

use prost_reflect::{DescriptorPool, MessageDescriptor, MethodDescriptor, ServiceDescriptor};

pub const REFLECTION_SERVICE_V1: &str = "grpc.reflection.v1.ServerReflection";
pub const REFLECTION_SERVICE_V1ALPHA: &str = "grpc.reflection.v1alpha.ServerReflection";

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("unknown service name")]
    UnknownServiceName,
    #[error("unknown RPC name")]
    UnknownRpcName,
    #[error("unknown symbol")]
    UnknownSymbol,
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
            .map(|fqsn| parse_fully_qualified_service_name(&fqsn).0)
            .collect();
        pkgs.sort();
        pkgs.dedup();
        pkgs
    }

    pub fn service(&self, fqsn: &str) -> Result<ServiceDescriptor> {
        self.pool
            .get_service_by_name(fqsn)
            .ok_or(Error::UnknownServiceName)
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

    pub fn resolve_message(&self, fqn: &str) -> Result<MessageDescriptor> {
        self.pool
            .get_message_by_name(fqn)
            .ok_or(Error::UnknownSymbol)
    }

    pub fn resolve_fully_qualified_method_name(&self, fqmn: &str) -> Result<(String, String)> {
        self.methods.get(fqmn).cloned().ok_or(Error::UnknownSymbol)
    }
}

pub fn parse_fully_qualified_service_name(fqsn: &str) -> (String, String) {
    match fqsn.rfind('.') {
        None => (String::new(), fqsn.to_string()),
        Some(i) => (fqsn[..i].to_string(), fqsn[i + 1..].to_string()),
    }
}

pub fn fully_qualified_service_name(pkg: &str, svc: &str) -> String {
    if pkg.is_empty() {
        svc.to_string()
    } else {
        format!("{pkg}.{svc}")
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
    fn parse_fqsn() {
        assert_eq!(
            parse_fully_qualified_service_name("a.b.Service"),
            ("a.b".to_string(), "Service".to_string())
        );
        assert_eq!(
            parse_fully_qualified_service_name("Service"),
            ("".to_string(), "Service".to_string())
        );
    }

    #[test]
    fn unknown_service() {
        let idl = idl();
        assert!(matches!(
            idl.service("test.Nope"),
            Err(Error::UnknownServiceName)
        ));
        assert!(matches!(
            idl.method("test.APIService", "Nope"),
            Err(Error::UnknownRpcName)
        ));
    }
}
