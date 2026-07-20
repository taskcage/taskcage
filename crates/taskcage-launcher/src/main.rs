#[cfg(not(target_os = "linux"))]
compile_error!("taskcage-launcher requires Linux");

use std::env;
use std::ffi::OsString;
use std::io;
use std::os::unix::process::CommandExt;
use std::process::{Command, ExitCode};

use rustix::process::{Signal, set_parent_process_death_signal};

fn main() -> ExitCode {
    match run() {
        Ok(never) => match never {},
        Err(error) => {
            eprintln!("taskcage-launcher: {error}");
            ExitCode::from(126)
        }
    }
}

fn run() -> io::Result<std::convert::Infallible> {
    let mut args = env::args_os().skip(1);
    let separator = args.next();
    if separator.as_deref() != Some(std::ffi::OsStr::new("--")) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "usage: taskcage-launcher -- <executable> [args...]",
        ));
    }

    let executable: OsString = args.next().ok_or_else(|| {
        io::Error::new(io::ErrorKind::InvalidInput, "missing target executable")
    })?;

    set_parent_process_death_signal(Some(Signal::KILL)).map_err(io::Error::from)?;

    let error = Command::new(executable).args(args).exec();
    Err(error)
}
