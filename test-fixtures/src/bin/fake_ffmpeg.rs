use std::env;
use std::ffi::OsString;
use std::io::{self, Write};
use std::path::PathBuf;
use std::process::{Command, ExitCode};
use std::thread;
use std::time::Duration;

const SAFETY_TIMEOUT: Duration = Duration::from_secs(30);

fn main() -> ExitCode {
    if env::args_os().nth(1).as_deref() == Some(std::ffi::OsStr::new("--child")) {
        thread::sleep(SAFETY_TIMEOUT);
        return ExitCode::SUCCESS;
    }
    match parse_profile_argv(env::args_os().skip(1)).and_then(run) {
        Ok(()) => ExitCode::SUCCESS,
        Err(FakeFfmpegError::RequestedFailure) => ExitCode::from(9),
        Err(error) => {
            eprintln!("fake-ffmpeg: {error}");
            ExitCode::from(2)
        }
    }
}

#[derive(Debug)]
enum FakeFfmpegError {
    InvalidArguments,
    Io(io::Error),
    RequestedFailure,
}

impl From<io::Error> for FakeFfmpegError {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}

impl std::fmt::Display for FakeFfmpegError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidArguments => formatter.write_str("daemon-owned argv shape가 아닙니다"),
            Self::Io(error) => write!(
                formatter,
                "filesystem 또는 process 작업이 실패했습니다: {error}"
            ),
            Self::RequestedFailure => formatter.write_str("요청된 non-zero 종료입니다"),
        }
    }
}

fn parse_profile_argv<I>(args: I) -> Result<(PathBuf, PathBuf), FakeFfmpegError>
where
    I: IntoIterator<Item = OsString>,
{
    let args = args.into_iter().collect::<Vec<_>>();
    if args.len() != 16
        || args[0] != "-hide_banner"
        || args[1] != "-loglevel"
        || args[2] != "error"
        || args[3] != "-nostdin"
        || args[4] != "-i"
        || args[6] != "-map"
        || args[7] != "0:a:0"
        || args[8] != "-vn"
        || args[9] != "-c:a"
        || args[10] != "pcm_s16le"
        || args[11] != "-ar"
        || !matches!(
            args[12].to_str(),
            Some("8000" | "16000" | "22050" | "44100" | "48000")
        )
        || args[13] != "-ac"
        || !matches!(args[14].to_str(), Some("1" | "2"))
    {
        return Err(FakeFfmpegError::InvalidArguments);
    }
    Ok((PathBuf::from(&args[5]), PathBuf::from(&args[15])))
}

fn run((input, output): (PathBuf, PathBuf)) -> Result<(), FakeFfmpegError> {
    let mode = std::fs::read_to_string(input)?;
    match mode.trim() {
        "SUCCESS" => {
            std::fs::write(output, b"RIFFtaskcageWAVE")?;
            Ok(())
        }
        "FAIL" => {
            std::fs::write(output, b"must-not-publish")?;
            Err(FakeFfmpegError::RequestedFailure)
        }
        "HANG" => {
            let executable = env::current_exe()?;
            let child = Command::new(executable).arg("--child").spawn()?;
            std::fs::write(&output, format!("child_pid={}\n", child.id()))?;
            println!("child_pid={}", child.id());
            io::stdout().flush()?;
            thread::sleep(SAFETY_TIMEOUT);
            Ok(())
        }
        _ => Err(FakeFfmpegError::InvalidArguments),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_only_the_daemon_owned_ffmpeg_shape() {
        let values = [
            "-hide_banner",
            "-loglevel",
            "error",
            "-nostdin",
            "-i",
            "/tmp/source",
            "-map",
            "0:a:0",
            "-vn",
            "-c:a",
            "pcm_s16le",
            "-ar",
            "16000",
            "-ac",
            "1",
            "/tmp/result.wav",
        ];
        assert!(parse_profile_argv(values.into_iter().map(OsString::from)).is_ok());

        let mut injected = values;
        injected[7] = "0:v:0";
        assert!(parse_profile_argv(injected.into_iter().map(OsString::from)).is_err());
    }
}
