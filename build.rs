fn main() {
    tonic_build::configure()
        .out_dir("src/analyzer_service/")
        .build_client(true)
        .compile_protos(&["src/build/proto/provider.proto"], &["src/build/proto/"])
        .unwrap();

    tonic_build::configure()
        .file_descriptor_set_path("src/analyzer_service/provider_service_descriptor.bin")
        .compile_protos(&["src/build/proto/provider.proto"], &["proto"])
        .unwrap();
}
