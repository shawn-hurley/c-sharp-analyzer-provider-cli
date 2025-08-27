mod analyzer_service;
mod c_sharp_graph;
mod pipe_stream;
mod provider;

use std::env::temp_dir;

use crate::analyzer_service::proto;
use crate::analyzer_service::provider_service_server::ProviderServiceServer;
use crate::provider::CSharpProvider;
use clap::{command, Parser};
use env_logger::Env;
use tokio::sync::Mutex;
use tonic::transport::Server;
use tracing::Level;

#[derive(Parser)]
#[command(version, about, long_about = None)]
struct Args {
    #[arg(long)]
    port: Option<usize>,

    #[arg(long)]
    socket: Option<String>,

    #[arg(long)]
    name: Option<String>,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    env_logger::Builder::from_env(Env::default().default_filter_or("trace")).init();
    //tracing_subscriber::fmt().init();
    let args = Args::parse();

    let provider = CSharpProvider {
        db_path: temp_dir().join("c_sharp_provider.db"),
        config: Mutex::new(None),
    };

    let service = tonic_reflection::server::Builder::configure()
        .register_encoded_file_descriptor_set(proto::FILE_DESCRIPTOR_SET)
        .build_v1()
        .unwrap();

    if args.port.is_some() {
        let s = format!("[::1]:{}", args.port.unwrap());
        println!("Using gRPC over HTTP/2 on port {}", s);

        let addr = s.parse()?;

        Server::builder()
            .add_service(ProviderServiceServer::new(provider))
            .add_service(service)
            .serve(addr)
            .await?;
    } else {
        #[cfg(not(windows))]
        {
            println!("Running on Unix-like OS");

            use tokio::net::UnixListener;
            use tokio_stream::wrappers::UnixListenerStream;

            let uds = UnixListener::bind(args.socket.unwrap())?;
            let uds_stream = UnixListenerStream::new(uds);

            Server::builder()
                .add_service(ProviderServiceServer::new(provider))
                .add_service(service)
                .serve_with_incoming(uds_stream)
                .await?;
        }
        #[cfg(target_os = "windows")]
        {
            println!("Using Windows OS");
            use crate::pipe_stream::get_named_pipe_connection_stream;
            Server::builder()
                .add_service(ProviderServiceServer::new(provider))
                .add_service(service)
                .serve_with_incoming(get_named_pipe_connection_stream(args.socket.unwrap()))
                .await?;
        }
    }

    Ok(())
}
