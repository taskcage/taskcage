//! Remote TLS listener와 principal deployment policy를 읽고 검증한다.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::fs::File;
use std::io::{self, BufReader};
use std::net::SocketAddr;
use std::num::{NonZeroU32, NonZeroU64, NonZeroUsize};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime};

use argon2::password_hash::PasswordHash;
use rustls::ServerConfig;
use rustls::pki_types::{CertificateDer, PrivateKeyDer};
use serde::Deserialize;
use thiserror::Error;

use crate::remote_protocol::{
    ProfileEffectiveResources, ProfileIdentity, ProfileResourceOverrides, REMOTE_MAX_FRAME_BYTES,
    REMOTE_PROTOCOL_VERSION, RemoteRequest, UploadArtifactChunkPayload, valid_client_id,
};
use crate::resource_budget::ResourceBudget;

pub const REMOTE_ALPN: &[u8] = b"taskcage/remote/1";

#[derive(Clone)]
pub struct RemoteDaemonConfig {
    pub source_path: PathBuf,
    pub listen_address: SocketAddr,
    pub max_remote_connections: NonZeroUsize,
    pub tls_handshake_timeout: Duration,
    pub authentication_timeout: Duration,
    pub idle_connection_timeout: Duration,
    pub session_lifetime: Duration,
    pub artifact_root: PathBuf,
    pub max_artifact_bytes: NonZeroU64,
    pub max_artifact_chunk_bytes: NonZeroU32,
    pub artifact_retention: Duration,
    pub principals: BTreeMap<String, PrincipalPolicy>,
    certificate_chain: Vec<CertificateDer<'static>>,
    tls: Arc<ServerConfig>,
}

impl fmt::Debug for RemoteDaemonConfig {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("RemoteDaemonConfig")
            .field("listen_address", &self.listen_address)
            .field("max_remote_connections", &self.max_remote_connections)
            .field("tls_handshake_timeout", &self.tls_handshake_timeout)
            .field("authentication_timeout", &self.authentication_timeout)
            .field("idle_connection_timeout", &self.idle_connection_timeout)
            .field("session_lifetime", &self.session_lifetime)
            .field("artifact_root", &self.artifact_root)
            .field("max_artifact_bytes", &self.max_artifact_bytes)
            .field("max_artifact_chunk_bytes", &self.max_artifact_chunk_bytes)
            .field("artifact_retention", &self.artifact_retention)
            .field("principals", &self.principals.keys().collect::<Vec<_>>())
            .finish_non_exhaustive()
    }
}

impl RemoteDaemonConfig {
    pub fn load(path: &Path) -> Result<Self, RemoteConfigError> {
        if !path.is_absolute() {
            return Err(RemoteConfigError::Invalid(
                "remote config 경로는 절대 경로여야 합니다".to_owned(),
            ));
        }
        validate_sensitive_file(path, "remote config")?;
        let bytes = std::fs::read(path).map_err(|source| RemoteConfigError::Read {
            path: path.to_path_buf(),
            source,
        })?;
        let wire: RemoteDaemonConfigFile =
            serde_json::from_slice(&bytes).map_err(RemoteConfigError::Json)?;
        wire.build(path.to_path_buf())
    }

    pub fn tls_acceptor(&self) -> tokio_rustls::TlsAcceptor {
        tokio_rustls::TlsAcceptor::from(Arc::clone(&self.tls))
    }

    pub fn certificate_chain(&self) -> &[CertificateDer<'static>] {
        &self.certificate_chain
    }
}

#[derive(Clone)]
pub struct PrincipalPolicy {
    pub client_id: String,
    pub secret_verifier: String,
    pub allowed_profiles: BTreeSet<ProfileIdentityKey>,
    pub maximum_resource_overrides: Option<ProfileEffectiveResources>,
    pub artifact_upload_allowed: bool,
    pub max_principal_artifact_bytes: NonZeroU64,
    pub max_principal_artifacts: NonZeroUsize,
}

impl fmt::Debug for PrincipalPolicy {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PrincipalPolicy")
            .field("client_id", &self.client_id)
            .field("secret_verifier", &"[REDACTED]")
            .field("allowed_profiles", &self.allowed_profiles)
            .field(
                "maximum_resource_overrides",
                &self.maximum_resource_overrides,
            )
            .field("artifact_upload_allowed", &self.artifact_upload_allowed)
            .field(
                "max_principal_artifact_bytes",
                &self.max_principal_artifact_bytes,
            )
            .field("max_principal_artifacts", &self.max_principal_artifacts)
            .finish()
    }
}

impl PrincipalPolicy {
    pub fn allows_profile(&self, profile: &ProfileIdentity) -> bool {
        self.allowed_profiles.contains(&ProfileIdentityKey {
            name: profile.name.clone(),
            version: profile.version.clone(),
        })
    }

    pub fn allows_resource_overrides(&self, overrides: Option<&ProfileResourceOverrides>) -> bool {
        let Some(overrides) = overrides else {
            return true;
        };
        let Some(maximum) = &self.maximum_resource_overrides else {
            return false;
        };
        overrides.limits.as_ref().is_none_or(|limits| {
            limits.cpu_max.as_ref().is_none_or(|cpu| {
                cpu_ratio_within_maximum(
                    cpu.quota_micros,
                    cpu.period_micros,
                    maximum.limits.cpu_max.quota_micros,
                    maximum.limits.cpu_max.period_micros,
                )
            }) && limits
                .memory_max_bytes
                .is_none_or(|value| value <= maximum.limits.memory_max_bytes)
                && limits
                    .pids_max
                    .is_none_or(|value| value <= maximum.limits.pids_max)
                && limits
                    .wall_time_limit_ms
                    .is_none_or(|value| value <= maximum.limits.wall_time_limit_ms)
        }) && overrides.output.as_ref().is_none_or(|output| {
            output
                .stdout_tail_max_bytes
                .is_none_or(|value| value <= maximum.output.stdout_tail_max_bytes)
                && output
                    .stderr_tail_max_bytes
                    .is_none_or(|value| value <= maximum.output.stderr_tail_max_bytes)
        })
    }
}

fn cpu_ratio_within_maximum(
    actual_quota: u64,
    actual_period: u64,
    maximum_quota: u64,
    maximum_period: u64,
) -> bool {
    u128::from(actual_quota) * u128::from(maximum_period)
        <= u128::from(maximum_quota) * u128::from(actual_period)
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct ProfileIdentityKey {
    pub name: String,
    pub version: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct RemoteDaemonConfigFile {
    listen_address: SocketAddr,
    tls: TlsFileConfig,
    max_remote_connections: usize,
    tls_handshake_timeout_ms: u64,
    authentication_timeout_ms: u64,
    idle_connection_timeout_ms: u64,
    session_lifetime_seconds: u64,
    artifact_root: PathBuf,
    max_artifact_bytes: u64,
    max_artifact_chunk_bytes: u32,
    artifact_retention_seconds: u64,
    principals: Vec<PrincipalPolicyFile>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct TlsFileConfig {
    certificate_chain_path: PathBuf,
    private_key_path: PathBuf,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct PrincipalPolicyFile {
    client_id: String,
    secret_verifier: String,
    allowed_profiles: Vec<ProfileIdentity>,
    #[serde(default)]
    maximum_resource_overrides: Option<ProfileEffectiveResources>,
    artifact_upload_allowed: bool,
    max_principal_artifact_bytes: u64,
    max_principal_artifacts: usize,
}

impl fmt::Debug for PrincipalPolicyFile {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PrincipalPolicyFile")
            .field("client_id", &self.client_id)
            .field("secret_verifier", &"[REDACTED]")
            .field("allowed_profiles", &self.allowed_profiles)
            .field(
                "maximum_resource_overrides",
                &self.maximum_resource_overrides,
            )
            .field("artifact_upload_allowed", &self.artifact_upload_allowed)
            .field(
                "max_principal_artifact_bytes",
                &self.max_principal_artifact_bytes,
            )
            .field("max_principal_artifacts", &self.max_principal_artifacts)
            .finish()
    }
}

impl RemoteDaemonConfigFile {
    fn build(self, source_path: PathBuf) -> Result<RemoteDaemonConfig, RemoteConfigError> {
        if !self.artifact_root.is_absolute() {
            return Err(RemoteConfigError::Invalid(
                "remote artifactRoot는 절대 경로여야 합니다".to_owned(),
            ));
        }
        if self.artifact_root.to_str().is_none() {
            return Err(RemoteConfigError::Invalid(
                "remote artifactRoot는 UTF-8 경로여야 합니다".to_owned(),
            ));
        }
        let max_remote_connections =
            positive_usize("maxRemoteConnections", self.max_remote_connections)?;
        let tls_handshake_timeout = positive_duration(
            "tlsHandshakeTimeoutMs",
            self.tls_handshake_timeout_ms,
            Duration::from_millis,
        )?;
        let authentication_timeout = positive_duration(
            "authenticationTimeoutMs",
            self.authentication_timeout_ms,
            Duration::from_millis,
        )?;
        let idle_connection_timeout = positive_duration(
            "idleConnectionTimeoutMs",
            self.idle_connection_timeout_ms,
            Duration::from_millis,
        )?;
        let session_lifetime = positive_duration(
            "sessionLifetimeSeconds",
            self.session_lifetime_seconds,
            Duration::from_secs,
        )?;
        let artifact_retention = positive_duration(
            "artifactRetentionSeconds",
            self.artifact_retention_seconds,
            Duration::from_secs,
        )?;
        let max_artifact_bytes = NonZeroU64::new(self.max_artifact_bytes)
            .ok_or_else(|| invalid_positive("maxArtifactBytes"))?;
        let max_artifact_chunk_bytes = NonZeroU32::new(self.max_artifact_chunk_bytes)
            .ok_or_else(|| invalid_positive("maxArtifactChunkBytes"))?;
        let maximum_frame_bytes = upload_chunk_frame_bytes(max_artifact_chunk_bytes.get())?;
        if maximum_frame_bytes > REMOTE_MAX_FRAME_BYTES {
            return Err(RemoteConfigError::Invalid(
                "maxArtifactChunkBytes의 base64 값은 Remote frame에 들어가야 합니다".to_owned(),
            ));
        }

        let mut principals = BTreeMap::new();
        for principal in self.principals {
            if !valid_client_id(&principal.client_id) {
                return Err(RemoteConfigError::Invalid(format!(
                    "잘못된 principal clientId입니다: {}",
                    principal.client_id
                )));
            }
            validate_argon2id(&principal.secret_verifier)?;
            if let Some(maximum) = &principal.maximum_resource_overrides {
                validate_maximum_resources(maximum)?;
            }
            let max_principal_artifact_bytes =
                NonZeroU64::new(principal.max_principal_artifact_bytes)
                    .ok_or_else(|| invalid_positive("maxPrincipalArtifactBytes"))?;
            let max_principal_artifacts = NonZeroUsize::new(principal.max_principal_artifacts)
                .ok_or_else(|| invalid_positive("maxPrincipalArtifacts"))?;
            let allowed_profiles = principal
                .allowed_profiles
                .into_iter()
                .map(|profile| ProfileIdentityKey {
                    name: profile.name,
                    version: profile.version,
                })
                .collect();
            let client_id = principal.client_id;
            let policy = PrincipalPolicy {
                client_id: client_id.clone(),
                secret_verifier: principal.secret_verifier,
                allowed_profiles,
                maximum_resource_overrides: principal.maximum_resource_overrides,
                artifact_upload_allowed: principal.artifact_upload_allowed,
                max_principal_artifact_bytes,
                max_principal_artifacts,
            };
            if principals.insert(client_id.clone(), policy).is_some() {
                return Err(RemoteConfigError::Invalid(format!(
                    "principal clientId가 중복되었습니다: {client_id}"
                )));
            }
        }
        if principals.is_empty() {
            return Err(RemoteConfigError::Invalid(
                "Remote listener에는 principal이 하나 이상 필요합니다".to_owned(),
            ));
        }

        let (tls, certificate_chain) = load_tls_config(&self.tls)?;
        Ok(RemoteDaemonConfig {
            source_path,
            listen_address: self.listen_address,
            max_remote_connections,
            tls_handshake_timeout,
            authentication_timeout,
            idle_connection_timeout,
            session_lifetime,
            artifact_root: self.artifact_root,
            max_artifact_bytes,
            max_artifact_chunk_bytes,
            artifact_retention,
            principals,
            certificate_chain,
            tls: Arc::new(tls),
        })
    }
}

fn validate_maximum_resources(
    maximum: &ProfileEffectiveResources,
) -> Result<(), RemoteConfigError> {
    ResourceBudget::try_from_protocol(
        crate::protocol::ResourceLimits {
            cpu_max: crate::protocol::CpuMax {
                quota_micros: maximum.limits.cpu_max.quota_micros,
                period_micros: maximum.limits.cpu_max.period_micros,
            },
            memory_max_bytes: maximum.limits.memory_max_bytes,
            pids_max: maximum.limits.pids_max,
            wall_time_limit_ms: maximum.limits.wall_time_limit_ms,
        },
        crate::protocol::OutputLimits {
            stdout_tail_max_bytes: maximum.output.stdout_tail_max_bytes,
            stderr_tail_max_bytes: maximum.output.stderr_tail_max_bytes,
        },
    )
    .map(|_| ())
    .map_err(|error| {
        RemoteConfigError::Invalid(format!(
            "principal maximumResourceOverrides가 잘못되었습니다: {error}"
        ))
    })
}

fn validate_argon2id(verifier: &str) -> Result<(), RemoteConfigError> {
    let parsed = PasswordHash::new(verifier).map_err(|_| {
        RemoteConfigError::Invalid("principal secretVerifier가 PHC 문자열이 아닙니다".to_owned())
    })?;
    if parsed.algorithm.as_str() != "argon2id" {
        return Err(RemoteConfigError::Invalid(
            "principal secretVerifier는 Argon2id여야 합니다".to_owned(),
        ));
    }
    let memory_kib = parsed.params.get_decimal("m");
    let iterations = parsed.params.get_decimal("t");
    let parallelism = parsed.params.get_decimal("p");
    if parsed.version != Some(19)
        || parsed.salt.is_none()
        || parsed.hash.is_none()
        || !memory_kib.is_some_and(|value| (19_456..=262_144).contains(&value))
        || !iterations.is_some_and(|value| (2..=10).contains(&value))
        || !parallelism.is_some_and(|value| (1..=16).contains(&value))
    {
        return Err(RemoteConfigError::Invalid(
            "principal secretVerifier는 제한된 Argon2id v19 salt/memory/time/parallelism을 사용해야 합니다"
                .to_owned(),
        ));
    }
    Ok(())
}

fn load_tls_config(
    config: &TlsFileConfig,
) -> Result<(ServerConfig, Vec<CertificateDer<'static>>), RemoteConfigError> {
    if !config.certificate_chain_path.is_absolute() || !config.private_key_path.is_absolute() {
        return Err(RemoteConfigError::Invalid(
            "TLS certificate와 private key 경로는 절대 경로여야 합니다".to_owned(),
        ));
    }
    validate_regular_file(&config.certificate_chain_path, "TLS certificate chain")?;
    validate_sensitive_file(&config.private_key_path, "TLS private key")?;
    let certificates = load_certificates(&config.certificate_chain_path)?;
    let certificate_chain = certificates.clone();
    let private_key = load_private_key(&config.private_key_path)?;
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let mut server = ServerConfig::builder_with_provider(provider)
        .with_protocol_versions(&[&rustls::version::TLS13])
        .map_err(|error| RemoteConfigError::Tls(error.to_string()))?
        .with_no_client_auth()
        .with_single_cert(certificates, private_key)
        .map_err(|error| RemoteConfigError::Tls(error.to_string()))?;
    server.alpn_protocols = vec![REMOTE_ALPN.to_vec()];
    Ok((server, certificate_chain))
}

fn validate_regular_file(path: &Path, label: &str) -> Result<(), RemoteConfigError> {
    validate_path_ancestors(path, label)?;
    let metadata = std::fs::symlink_metadata(path).map_err(|source| RemoteConfigError::Read {
        path: path.to_path_buf(),
        source,
    })?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err(RemoteConfigError::Invalid(format!(
            "{label}는 symlink가 아닌 regular file이어야 합니다"
        )));
    }
    Ok(())
}

pub(crate) fn validate_path_ancestors(path: &Path, label: &str) -> Result<(), RemoteConfigError> {
    #[cfg(target_os = "linux")]
    {
        use std::os::unix::fs::PermissionsExt;
        for ancestor in path.parent().into_iter().flat_map(Path::ancestors) {
            let metadata =
                std::fs::symlink_metadata(ancestor).map_err(|source| RemoteConfigError::Read {
                    path: ancestor.to_path_buf(),
                    source,
                })?;
            if metadata.file_type().is_symlink() || !metadata.is_dir() {
                return Err(RemoteConfigError::Invalid(format!(
                    "{label} 상위 경로는 symlink가 아닌 directory여야 합니다: {}",
                    ancestor.display()
                )));
            }
            let mode = metadata.permissions().mode();
            if mode & 0o022 != 0 && mode & 0o1000 == 0 {
                return Err(RemoteConfigError::Invalid(format!(
                    "{label} 상위 경로가 sticky bit 없이 group/other writable입니다: {}",
                    ancestor.display()
                )));
            }
        }
    }
    #[cfg(not(target_os = "linux"))]
    let _ = (path, label);
    Ok(())
}

fn validate_sensitive_file(path: &Path, label: &str) -> Result<(), RemoteConfigError> {
    validate_regular_file(path, label)?;
    #[cfg(target_os = "linux")]
    {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};
        let metadata =
            std::fs::symlink_metadata(path).map_err(|source| RemoteConfigError::Read {
                path: path.to_path_buf(),
                source,
            })?;
        if metadata.uid() != unsafe { libc::geteuid() }
            || metadata.permissions().mode() & 0o077 != 0
        {
            return Err(RemoteConfigError::Invalid(format!(
                "{label}는 service UID 소유이며 group/other 권한이 없어야 합니다"
            )));
        }
    }
    Ok(())
}

fn load_certificates(path: &Path) -> Result<Vec<CertificateDer<'static>>, RemoteConfigError> {
    let file = File::open(path).map_err(|source| RemoteConfigError::Read {
        path: path.to_path_buf(),
        source,
    })?;
    let certificates = rustls_pemfile::certs(&mut BufReader::new(file))
        .collect::<Result<Vec<_>, _>>()
        .map_err(|source| RemoteConfigError::Read {
            path: path.to_path_buf(),
            source,
        })?;
    if certificates.is_empty() {
        return Err(RemoteConfigError::Tls(
            "certificate chain이 비어 있습니다".to_owned(),
        ));
    }
    Ok(certificates)
}

fn load_private_key(path: &Path) -> Result<PrivateKeyDer<'static>, RemoteConfigError> {
    let file = File::open(path).map_err(|source| RemoteConfigError::Read {
        path: path.to_path_buf(),
        source,
    })?;
    rustls_pemfile::private_key(&mut BufReader::new(file))
        .map_err(|source| RemoteConfigError::Read {
            path: path.to_path_buf(),
            source,
        })?
        .ok_or_else(|| RemoteConfigError::Tls("private key가 없습니다".to_owned()))
}

fn upload_chunk_frame_bytes(raw_bytes: u32) -> Result<usize, RemoteConfigError> {
    let base64_bytes = usize::try_from(raw_bytes)
        .ok()
        .and_then(|bytes| bytes.checked_add(2))
        .and_then(|bytes| (bytes / 3).checked_mul(4))
        .ok_or_else(|| {
            RemoteConfigError::Invalid("maxArtifactChunkBytes가 너무 큽니다".to_owned())
        })?;
    let envelope = RemoteRequest::UploadArtifactChunk {
        remote_protocol_version: REMOTE_PROTOCOL_VERSION,
        request_id: "ffffffff-ffff-ffff-ffff-ffffffffffff".to_owned(),
        payload: UploadArtifactChunkPayload {
            artifact_id: "ffffffff-ffff-ffff-ffff-ffffffffffff".to_owned(),
            offset: u64::MAX,
            data_base64: String::new(),
        },
    };
    serde_json::to_vec(&envelope)
        .map_err(RemoteConfigError::Json)?
        .len()
        .checked_add(base64_bytes)
        .ok_or_else(|| {
            RemoteConfigError::Invalid("maxArtifactChunkBytes frame이 너무 큽니다".to_owned())
        })
}

fn positive_usize(name: &str, value: usize) -> Result<NonZeroUsize, RemoteConfigError> {
    NonZeroUsize::new(value).ok_or_else(|| invalid_positive(name))
}

fn positive_duration(
    name: &str,
    value: u64,
    convert: impl FnOnce(u64) -> Duration,
) -> Result<Duration, RemoteConfigError> {
    if value == 0 {
        return Err(invalid_positive(name));
    }
    let duration = convert(value);
    if Instant::now().checked_add(duration).is_none()
        || SystemTime::now().checked_add(duration).is_none()
    {
        return Err(RemoteConfigError::Invalid(format!(
            "{name} 값은 이 플랫폼에서 표현할 수 없습니다"
        )));
    }
    Ok(duration)
}

fn invalid_positive(name: &str) -> RemoteConfigError {
    RemoteConfigError::Invalid(format!("{name} 값은 0보다 커야 합니다"))
}

#[derive(Debug, Error)]
pub enum RemoteConfigError {
    #[error("Remote config를 읽지 못했습니다: {path}")]
    Read {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("Remote config JSON이 잘못되었습니다")]
    Json(#[source] serde_json::Error),
    #[error("Remote config가 잘못되었습니다: {0}")]
    Invalid(String),
    #[error("Remote TLS 설정이 잘못되었습니다: {0}")]
    Tls(String),
}

#[cfg(test)]
mod tests {
    use argon2::Argon2;
    use argon2::password_hash::{PasswordHasher, SaltString};

    use std::time::Duration;

    use crate::remote_protocol::{CpuMax, OutputLimits, ProfileEffectiveResources, ResourceLimits};

    use super::{
        cpu_ratio_within_maximum, positive_duration, upload_chunk_frame_bytes, validate_argon2id,
        validate_maximum_resources,
    };

    #[test]
    fn principal_cpu_override_uses_the_exact_ratio() {
        assert!(cpu_ratio_within_maximum(25_000, 50_000, 50_000, 100_000));
        assert!(!cpu_ratio_within_maximum(40_000, 50_000, 50_000, 100_000));
        assert!(cpu_ratio_within_maximum(
            u64::MAX,
            u64::MAX,
            u64::MAX,
            u64::MAX
        ));
    }

    #[test]
    fn deployment_durations_must_fit_platform_deadlines() {
        assert!(positive_duration("deadline", 1, Duration::from_millis).is_ok());
        assert!(positive_duration("deadline", u64::MAX, Duration::from_secs).is_err());
    }

    #[test]
    fn principal_override_maximum_must_be_a_valid_complete_budget() {
        let mut maximum = ProfileEffectiveResources {
            limits: ResourceLimits {
                cpu_max: CpuMax {
                    quota_micros: 50_000,
                    period_micros: 100_000,
                },
                memory_max_bytes: 64 * 1024 * 1024,
                pids_max: 8,
                wall_time_limit_ms: 5_000,
            },
            output: OutputLimits {
                stdout_tail_max_bytes: 1_024,
                stderr_tail_max_bytes: 1_024,
            },
        };
        assert!(validate_maximum_resources(&maximum).is_ok());
        maximum.limits.cpu_max.period_micros = 0;
        assert!(validate_maximum_resources(&maximum).is_err());
    }

    #[test]
    fn chunk_limit_accounts_for_the_complete_json_frame() {
        assert!(upload_chunk_frame_bytes(780_000).unwrap() <= super::REMOTE_MAX_FRAME_BYTES);
        assert!(upload_chunk_frame_bytes(786_432).unwrap() > super::REMOTE_MAX_FRAME_BYTES);
    }

    #[test]
    fn verifier_policy_requires_salted_memory_hard_argon2id() {
        let salt = SaltString::encode_b64(b"remote-config-test-salt").expect("test salt");
        let verifier = Argon2::default()
            .hash_password(b"test-only-secret", &salt)
            .expect("default verifier")
            .to_string();
        assert!(validate_argon2id(&verifier).is_ok());
        assert!(
            validate_argon2id(
                "$argon2id$v=19$m=8,t=1,p=1$c2FsdC1mb3ItdGVzdA$u1FGeu2AahxZdZmkrOJioBgcvDvGiwSu9tSlA6aLJkI"
            )
            .is_err()
        );
        assert!(
            validate_argon2id(
                "$argon2i$v=19$m=19456,t=2,p=1$c2FsdC1mb3ItdGVzdA$u1FGeu2AahxZdZmkrOJioBgcvDvGiwSu9tSlA6aLJkI"
            )
            .is_err()
        );
    }
}
