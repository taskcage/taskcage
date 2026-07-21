use std::env;
use std::io::{self, Write};
use std::process::ExitCode;
use std::thread;

const CHUNK_BYTES: usize = 8 * 1024;
const CHUNK_COUNT: usize = 256;

fn main() -> ExitCode {
    let mode = env::args().nth(1).unwrap_or_else(|| "both".to_owned());
    let result = match mode.as_str() {
        "both" => write_both(),
        "stdout" => write_stdout(),
        "stderr" => write_stderr(),
        _ => {
            eprintln!("usage: output-flood [both|stdout|stderr]");
            return ExitCode::from(2);
        }
    };

    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("output flood failed: {error}");
            ExitCode::FAILURE
        }
    }
}

fn write_both() -> io::Result<()> {
    let stdout = thread::spawn(write_stdout);
    let stderr = thread::spawn(write_stderr);
    join_writer(stdout)?;
    join_writer(stderr)
}

fn write_stdout() -> io::Result<()> {
    let mut output = io::stdout().lock();
    write_flood(&mut output, b'O', b"STDOUT-END\n")
}

fn write_stderr() -> io::Result<()> {
    let mut output = io::stderr().lock();
    write_flood(&mut output, b'E', b"STDERR-END\n")
}

fn write_flood(output: &mut impl Write, byte: u8, marker: &[u8]) -> io::Result<()> {
    let chunk = [byte; CHUNK_BYTES];
    for _ in 0..CHUNK_COUNT {
        output.write_all(&chunk)?;
    }
    output.write_all(marker)?;
    output.flush()
}

fn join_writer(writer: thread::JoinHandle<io::Result<()>>) -> io::Result<()> {
    writer
        .join()
        .map_err(|_| io::Error::other("writer thread panicked"))?
}
