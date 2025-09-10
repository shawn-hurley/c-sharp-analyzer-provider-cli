mod analyzer_service;
mod c_sharp_graph;
mod pipe_stream;
mod provider;

use std::{env::temp_dir, path::PathBuf};

use crate::analyzer_service::proto;
use crate::analyzer_service::provider_service_server::ProviderServiceServer;
use crate::provider::CSharpProvider;
use clap::{command, Parser};
use tonic::transport::Server;
use tracing::info;

#[derive(Parser)]
#[command(version, about, long_about = None)]
struct Args {
    #[arg(long)]
    port: Option<usize>,

    #[arg(long)]
    socket: Option<String>,

    #[arg(long)]
    name: Option<String>,
    #[arg(long)]
    log_file: Option<String>,
    #[command(flatten)]
    verbosity: clap_verbosity_flag::Verbosity,
    #[arg(long)]
    db_path: Option<PathBuf>,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args = Args::parse();

    // construct a subscriber that prints formatted traces to stdout
    let subscriber = tracing_subscriber::FmtSubscriber::builder()
        .with_max_level(tracing::Level::DEBUG)
        .finish();
    // use that subscriber to process traces emitted after this point
    tracing::subscriber::set_global_default(subscriber)?;

    let provider = CSharpProvider::new(
        args.db_path
            .map_or(temp_dir().join("c_sharp_provider.db"), |x| x),
    );
    let service = tonic_reflection::server::Builder::configure()
        .register_encoded_file_descriptor_set(proto::FILE_DESCRIPTOR_SET)
        .build_v1alpha()
        .unwrap();

    if args.port.is_some() {
        let s = format!("[::1]:{}", args.port.unwrap());
        info!("Using gRPC over HTTP/2 on port {}", s);

        let addr = s.parse()?;

        Server::builder()
            .add_service(ProviderServiceServer::new(provider))
            .add_service(service)
            .serve(addr)
            .await?;
    } else {
        info!("using uds");
        #[cfg(not(windows))]
        {
            debug!("Running on Unix-like OS");

            use tokio::net::UnixListener;
            use tokio_stream::wrappers::UnixListenerStream;
            use tracing::debug;

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
            debug!("Using Windows OS");
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
