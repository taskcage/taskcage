use std::ffi::OsString;
use std::path::PathBuf;
use std::time::Duration;

use taskcaged::Error;

use super::{parse_number, required_option};

const DEFAULT_STATUS_TIMEOUT_MILLIS: u64 = 2_000;

#[derive(Debug)]
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub(crate) struct Config {
    socket_path: PathBuf,
    timeout: Duration,
}

pub(crate) fn parse(args: Vec<OsString>) -> taskcaged::Result<Config> {
    let mut socket_path = None;
    let mut timeout_ms = None;
    let mut index = 0;
    while index < args.len() {
        let name = args[index].to_str().ok_or_else(|| {
            Error::InvalidArgument("status 옵션 이름은 UTF-8이어야 합니다".to_owned())
        })?;
        let value = args
            .get(index + 1)
            .ok_or_else(|| Error::InvalidArgument(format!("{name} 옵션 값이 없습니다")))?;
        match name {
            "--socket" if socket_path.is_none() => socket_path = Some(PathBuf::from(value)),
            "--timeout-ms" if timeout_ms.is_none() => {
                timeout_ms = Some(parse_number(name, value)?);
            }
            "--socket" | "--timeout-ms" => {
                return Err(Error::InvalidArgument(format!(
                    "status 옵션이 중복되었습니다: {name}"
                )));
            }
            _ => {
                return Err(Error::InvalidArgument(format!(
                    "알 수 없는 status 옵션입니다: {name}"
                )));
            }
        }
        index += 2;
    }
    let socket_path = required_option("socket", socket_path)?;
    if !socket_path.is_absolute() {
        return Err(Error::InvalidArgument(
            "status socket 경로는 절대 경로여야 합니다".to_owned(),
        ));
    }
    let timeout_ms = timeout_ms.unwrap_or(DEFAULT_STATUS_TIMEOUT_MILLIS);
    if timeout_ms == 0 {
        return Err(Error::InvalidArgument(
            "status timeout-ms 값은 0보다 커야 합니다".to_owned(),
        ));
    }
    Ok(Config {
        socket_path,
        timeout: Duration::from_millis(timeout_ms),
    })
}

pub(crate) async fn execute(config: Config) -> taskcaged::Result<()> {
    #[cfg(target_os = "linux")]
    {
        let report = taskcaged::status::check(&config.socket_path, config.timeout).await?;
        println!("{}", serde_json::to_string(&report)?);
        if report.is_ready() {
            Ok(())
        } else {
            Err(Error::DaemonUnready)
        }
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = config;
        Err(Error::UnsupportedPlatform)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_requires_absolute_socket_and_uses_bounded_default_timeout() {
        let socket = std::env::temp_dir().join("taskcaged.sock");
        let config = parse(vec![
            OsString::from("--socket"),
            socket.clone().into_os_string(),
        ])
        .unwrap();
        assert_eq!(config.socket_path, socket);
        assert_eq!(config.timeout, Duration::from_millis(2_000));

        let error = parse(vec![
            OsString::from("--socket"),
            OsString::from("relative.sock"),
        ])
        .unwrap_err();
        assert!(error.to_string().contains("절대 경로"));
    }

    #[test]
    fn status_rejects_zero_timeout_and_duplicate_options() {
        let socket = std::env::temp_dir().join("taskcaged.sock");
        let error = parse(vec![
            OsString::from("--socket"),
            socket.clone().into_os_string(),
            OsString::from("--timeout-ms"),
            OsString::from("0"),
        ])
        .unwrap_err();
        assert!(error.to_string().contains("0보다 커야"));

        let error = parse(vec![
            OsString::from("--socket"),
            socket.clone().into_os_string(),
            OsString::from("--socket"),
            socket.into_os_string(),
        ])
        .unwrap_err();
        assert!(error.to_string().contains("중복"));
    }
}
