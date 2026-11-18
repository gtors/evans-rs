fn main() {
    println!("cargo:rerun-if-changed=testdata/proto/test.proto");

    let fds = protox::compile(["testdata/proto/test.proto"], ["testdata/proto"])
        .expect("failed to compile the e2e test proto");
    let mut config = prost_build::Config::new();
    config.enable_type_names();
    tonic_prost_build::configure()
        .build_server(true)
        .compile_fds_with_config(fds, config)
        .expect("failed to generate the e2e test server code");
}
