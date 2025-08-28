#[cfg(target_os = "windows")]
mod server;

#[cfg(target_os = "windows")]
pub use server::NamedPipeConnection;
#[cfg(target_os = "windows")]
pub use server::get_named_pipe_connection_stream;
