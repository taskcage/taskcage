use std::ffi::{OsStr, OsString};

pub(crate) mod bundle;
pub(crate) mod capsule;
pub(crate) mod package;
pub(crate) mod run_once;
pub(crate) mod secret;
pub(crate) mod serve;
pub(crate) mod status;

use taskcaged::Error;

#[allow(
    clippy::large_enum_variant,
    reason = "각 CLI config의 기존 값 소유와 command dispatch 순서를 그대로 유지합니다"
)]
pub(crate) enum Command {
    Serve(taskcaged::DaemonConfig),
    CheckEnvironment,
    Status(status::Config),
    RunOnce(taskcaged::RunOnceConfig),
    ImportPackage(package::Config),
    Bundle(bundle::Command),
    Capsule(capsule::Command),
    HashRemoteSecret,
}

pub(crate) fn parse(args: impl IntoIterator<Item = OsString>) -> taskcaged::Result<Command> {
    let mut args = args.into_iter();
    match args.next().as_deref() {
        None => Err(Error::InvalidArgument(
            "서비스 실행에는 serve와 명시적 socket 설정이 필요합니다".to_owned(),
        )),
        Some(command) if command == OsStr::new("serve") => {
            serve::parse(args.collect()).map(Command::Serve)
        }
        Some(command) if command == OsStr::new("check-environment") => {
            if args.next().is_some() {
                return Err(Error::InvalidArgument(
                    "check-environment 뒤에는 인자를 받을 수 없습니다".to_owned(),
                ));
            }
            Ok(Command::CheckEnvironment)
        }
        Some(command) if command == OsStr::new("status") => {
            status::parse(args.collect()).map(Command::Status)
        }
        Some(command) if command == OsStr::new("run-once") => {
            run_once::parse(args.collect()).map(Command::RunOnce)
        }
        Some(command) if command == OsStr::new("import-package") => {
            package::parse(args.collect()).map(Command::ImportPackage)
        }
        Some(command) if command == OsStr::new("bundle") => {
            bundle::parse(args.collect()).map(Command::Bundle)
        }
        Some(command) if command == OsStr::new("capsule") => {
            capsule::parse(args.collect()).map(Command::Capsule)
        }
        Some(command) if command == OsStr::new("hash-remote-secret") => {
            secret::parse(args.collect())?;
            Ok(Command::HashRemoteSecret)
        }
        Some(other) => Err(Error::InvalidArgument(format!(
            "알 수 없는 명령입니다: {other:?}; serve, check-environment, status, run-once, import-package, bundle, capsule 또는 hash-remote-secret을 사용하세요"
        ))),
    }
}

pub(super) fn required_option<T>(name: &str, value: Option<T>) -> taskcaged::Result<T> {
    value.ok_or_else(|| Error::InvalidArgument(format!("{name} 옵션은 필수입니다")))
}

pub(super) fn parse_number<T>(name: &str, value: &OsStr) -> taskcaged::Result<T>
where
    T: std::str::FromStr,
    T::Err: std::fmt::Display,
{
    let value = value
        .to_str()
        .ok_or_else(|| Error::InvalidArgument(format!("{name} 값은 UTF-8이어야 합니다")))?;
    value
        .parse()
        .map_err(|error| Error::InvalidArgument(format!("잘못된 {name} 값입니다: {error}")))
}
