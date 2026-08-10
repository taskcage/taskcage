use std::env;
use std::ffi::OsString;
use std::path::PathBuf;
use std::process::{Command, ExitCode};
use std::thread;
use std::time::Duration;

const SAFETY_TIMEOUT: Duration = Duration::from_secs(30);

fn main() -> ExitCode {
    match parse_args().and_then(run) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("ffmpeg-tree: {error}");
            ExitCode::from(2)
        }
    }
}

fn parse_args() -> Result<(PathBuf, PathBuf), String> {
    parse_values(env::args_os().skip(1))
}

fn parse_values<I>(mut args: I) -> Result<(PathBuf, PathBuf), String>
where
    I: Iterator<Item = OsString>,
{
    let mut ffmpeg = None;
    let mut ready = None;
    while let Some(name) = args.next() {
        let value = args
            .next()
            .ok_or_else(|| format!("{} 뒤에 값이 필요합니다", name.to_string_lossy()))?;
        match name.to_str() {
            Some("--ffmpeg") if ffmpeg.is_none() => ffmpeg = Some(PathBuf::from(value)),
            Some("--ready") if ready.is_none() => ready = Some(PathBuf::from(value)),
            Some("--ffmpeg" | "--ready") => {
                return Err(format!("{} 옵션이 중복되었습니다", name.to_string_lossy()));
            }
            _ => return Err(format!("알 수 없는 옵션입니다: {}", name.to_string_lossy())),
        }
    }

    let ffmpeg = ffmpeg.ok_or_else(|| "--ffmpeg 옵션은 필수입니다".to_owned())?;
    let ready = ready.ok_or_else(|| "--ready 옵션은 필수입니다".to_owned())?;
    if !ffmpeg.is_absolute() || !ready.is_absolute() {
        return Err("ffmpeg와 ready 경로는 절대 경로여야 합니다".to_owned());
    }
    Ok((ffmpeg, ready))
}

fn run((ffmpeg, ready): (PathBuf, PathBuf)) -> Result<(), String> {
    let mut child = Command::new(ffmpeg)
        .args([
            "-hide_banner",
            "-loglevel",
            "error",
            "-nostdin",
            "-re",
            "-f",
            "lavfi",
            "-i",
            "testsrc=size=64x64:rate=30",
            "-t",
            "30",
            "-threads",
            "1",
            "-f",
            "null",
            "-",
        ])
        .spawn()
        .map_err(|error| format!("FFmpeg child를 시작하지 못했습니다: {error}"))?;

    std::fs::write(&ready, format!("ffmpeg_pid={}\n", child.id()))
        .map_err(|error| format!("ready 파일을 기록하지 못했습니다: {error}"))?;

    thread::sleep(SAFETY_TIMEOUT);
    let _ = child.kill();
    let _ = child.wait();
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parser_requires_exact_absolute_paths() {
        let base = env::current_dir().unwrap();
        let error = parse_test_values(["--ffmpeg", "relative", "--ready", "relative-ready"])
            .expect_err("relative ffmpeg must fail");
        assert!(error.contains("절대 경로"));

        let ffmpeg = base.join("ffmpeg");
        let ready = base.join("ready");
        let parsed = parse_values(
            [
                OsString::from("--ffmpeg"),
                ffmpeg.clone().into_os_string(),
                OsString::from("--ready"),
                ready.clone().into_os_string(),
            ]
            .into_iter(),
        )
        .expect("absolute paths must pass");
        assert_eq!(parsed.0, ffmpeg);
        assert_eq!(parsed.1, ready);
    }

    fn parse_test_values<const N: usize>(values: [&str; N]) -> Result<(PathBuf, PathBuf), String> {
        parse_values(values.into_iter().map(OsString::from))
    }
}
