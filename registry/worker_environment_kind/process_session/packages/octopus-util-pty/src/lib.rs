pub mod pipe;
mod process;
pub mod process_group;
#[cfg(target_os = "macos")]
pub mod owned_process_group;
pub mod pty;
#[cfg(test)]
mod tests;
#[cfg(unix)]
mod unix_io;
#[cfg(windows)]
mod win;
#[cfg(windows)]
mod windows_input;

pub const DEFAULT_OUTPUT_BYTES_CAP: usize = 1024 * 1024;

pub use pipe::spawn_process as spawn_pipe_process;
pub use pipe::spawn_process_no_stdin as spawn_pipe_process_no_stdin;
pub use process::combine_output_receivers;
pub use process::spawn_from_driver;
pub use process::ProcessDriver;
pub use process::ProcessHandle;
pub use process::ProcessSignal;
pub use process::SpawnedProcess;
pub use process::TerminalSize;
pub type ExecCommandSession = ProcessHandle;
pub type SpawnedPty = SpawnedProcess;
pub use pty::conpty_supported;
pub use pty::spawn_process as spawn_pty_process;
#[cfg(windows)]
pub use win::conpty::RawConPty;
#[cfg(windows)]
pub use win::JobObject;
#[cfg(windows)]
pub use win::PsuedoCon;
#[cfg(windows)]
pub use windows_input::WindowsTtyInputNormalizer;
