use std::io::Read;

use taskcaged::Error;

pub(crate) fn parse(args: Vec<std::ffi::OsString>) -> taskcaged::Result<()> {
    if args.is_empty() {
        Ok(())
    } else {
        Err(Error::InvalidArgument(
            "hash-remote-secret 뒤에는 인자를 받을 수 없습니다".to_owned(),
        ))
    }
}

pub(crate) fn execute() -> taskcaged::Result<()> {
    let mut secret = zeroize::Zeroizing::new(Vec::new());
    std::io::stdin()
        .take(4_097)
        .read_to_end(&mut secret)
        .map_err(|error| {
            Error::InvalidArgument(format!("stdin secret을 읽지 못했습니다: {error}"))
        })?;
    if !(1..=4_096).contains(&secret.len()) {
        return Err(Error::InvalidArgument(
            "Remote secret은 1~4096 bytes여야 합니다".to_owned(),
        ));
    }
    use argon2::password_hash::{PasswordHasher, SaltString};
    let salt = SaltString::generate(&mut rand_core::OsRng);
    let verifier = argon2::Argon2::default()
        .hash_password(&secret, &salt)
        .map_err(|error| {
            Error::InvalidArgument(format!("secret verifier 생성에 실패했습니다: {error}"))
        })?;
    println!("{verifier}");
    Ok(())
}
