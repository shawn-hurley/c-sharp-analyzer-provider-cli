// Using example code from
// https://github.com/catalinsh/tonic-named-pipe-example
use std::{io, pin::Pin};
use futures_core::Stream;
use async_stream::stream;
use tokio::{
    io::{AsyncRead, AsyncWrite},
    net::windows::named_pipe::{NamedPipeServer, PipeMode, ServerOptions},
};
use tonic::transport::server::Connected;

pub struct NamedPipeConnection {
    inner: NamedPipeServer,
}

impl NamedPipeConnection {
    pub fn new(inner: NamedPipeServer) -> Self {
        Self { inner }
    }
}

impl Connected for NamedPipeConnection {
    type ConnectInfo = ();

    fn connect_info(&self) -> Self::ConnectInfo {
        ()
    }
}

impl AsyncRead for NamedPipeConnection {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &mut tokio::io::ReadBuf<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        let x = Pin::new(&mut self.inner).poll_read(cx, buf);
        if x.is_ready() {
            println!("buffer: {:?}", buf)
        }
        return x;
    }
}

impl AsyncWrite for NamedPipeConnection {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &[u8],
    ) -> std::task::Poll<Result<usize, std::io::Error>> {
        Pin::new(&mut self.inner).poll_write(cx, buf)
    }

    fn poll_flush(
        mut self: Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Result<(), std::io::Error>> {
        Pin::new(&mut self.inner).poll_flush(cx)
    }

    fn poll_shutdown(
        mut self: Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Result<(), std::io::Error>> {
        Pin::new(&mut self.inner).poll_shutdown(cx)
    }
}

pub fn get_named_pipe_connection_stream(
    name: String
) -> impl Stream<Item = io::Result<NamedPipeConnection>> {
    stream!{
        let mut server = ServerOptions::new()
            .first_pipe_instance(true)
            .pipe_mode(PipeMode::Byte)
            .create(&name)?;

        loop {
            server.connect().await?;

            let connection = NamedPipeConnection::new(server);
            yield Ok(connection);
            server = ServerOptions::new().create(&name)?;
        }
    }
}
