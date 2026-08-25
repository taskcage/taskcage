use std::collections::{BTreeMap, BTreeSet};
use std::num::{NonZeroU64, NonZeroUsize};
#[cfg(target_os = "linux")]
use std::sync::Arc;
#[cfg(target_os = "linux")]
use std::sync::atomic::{AtomicBool, Ordering};

use argon2::Argon2;
use argon2::password_hash::{PasswordHasher, SaltString};
#[cfg(target_os = "linux")]
use sha2::Digest;
use taskcaged::remote_auth::CredentialStore;
use taskcaged::remote_config::{PrincipalPolicy, ProfileIdentityKey};
#[cfg(target_os = "linux")]
use taskcaged::remote_dispatch::{RemoteBoolFuture, RemoteTaskBackend, RemoteTaskFuture};
#[cfg(target_os = "linux")]
use taskcaged::remote_protocol::{
    BeginArtifactUploadPayload, RemoteErrorCode, RemoteProfileRequestPayload, RemoteRequest,
    RemoteResponse, TaskIdPayload, UploadArtifactChunkPayload,
};

fn policy(client_id: &str, secret: &str) -> PrincipalPolicy {
    let salt = SaltString::encode_b64(b"remote-auth-test-salt").expect("test salt");
    let verifier = Argon2::default()
        .hash_password(secret.as_bytes(), &salt)
        .expect("test verifier")
        .to_string();
    PrincipalPolicy {
        client_id: client_id.to_owned(),
        secret_verifier: verifier,
        allowed_profiles: BTreeSet::from([ProfileIdentityKey {
            name: "ffmpeg-audio-to-wav".to_owned(),
            version: "1.0.0".to_owned(),
        }]),
        allow_all_installed_capsules: false,
        maximum_resource_overrides: None,
        artifact_upload_allowed: true,
        max_principal_artifact_bytes: NonZeroU64::new(10_000).expect("positive"),
        max_principal_artifacts: NonZeroUsize::new(2).expect("positive"),
    }
}

#[test]
fn authentication_hides_unknown_principals_and_wrong_secrets() {
    let store = CredentialStore::new(BTreeMap::from([(
        "document-worker".to_owned(),
        policy("document-worker", "correct secret"),
    )]));
    assert!(
        store
            .authenticate("unknown-worker", "correct secret")
            .is_none()
    );
    assert!(
        store
            .authenticate("document-worker", "wrong secret")
            .is_none()
    );
    assert!(
        store
            .authenticate("document-worker", "correct secret")
            .is_some()
    );
}

#[tokio::test]
async fn rotation_preserves_existing_session_and_revocation_closes_it() {
    let store = CredentialStore::new(BTreeMap::from([(
        "document-worker".to_owned(),
        policy("document-worker", "old secret"),
    )]));
    let mut existing = store
        .authenticate("document-worker", "old secret")
        .expect("old credential");
    store.rotate(policy("document-worker", "new secret"));
    assert!(
        store
            .authenticate("document-worker", "old secret")
            .is_none()
    );
    assert!(
        store
            .authenticate("document-worker", "new secret")
            .is_some()
    );

    store.revoke("document-worker");
    tokio::time::timeout(std::time::Duration::from_secs(1), existing.revoked())
        .await
        .expect("existing session should observe revocation");
    assert!(
        store
            .authenticate("document-worker", "new secret")
            .is_none()
    );
}

#[cfg(target_os = "linux")]
struct RejectingBackend {
    submit_started: tokio::sync::Notify,
    submit_release: tokio::sync::Notify,
    submit_completed: AtomicBool,
}

#[cfg(target_os = "linux")]
impl RemoteTaskBackend for RejectingBackend {
    fn submit<'a>(
        &'a self,
        _principal: &'a PrincipalPolicy,
        request_id: String,
        _payload: RemoteProfileRequestPayload,
        _artifacts: &'a taskcaged::remote_artifact::RemoteArtifactStore,
    ) -> RemoteTaskFuture<'a> {
        Box::pin(async move {
            self.submit_started.notify_one();
            self.submit_release.notified().await;
            self.submit_completed.store(true, Ordering::SeqCst);
            taskcaged::remote_server::error_response(
                request_id,
                RemoteErrorCode::InternalError,
                "test handler",
                false,
            )
        })
    }

    fn get<'a>(
        &'a self,
        _principal: &'a PrincipalPolicy,
        request_id: String,
        _payload: TaskIdPayload,
        _artifacts: &'a taskcaged::remote_artifact::RemoteArtifactStore,
    ) -> RemoteTaskFuture<'a> {
        Box::pin(async move {
            taskcaged::remote_server::error_response(
                request_id,
                RemoteErrorCode::TaskNotFound,
                "task was not found",
                false,
            )
        })
    }

    fn cancel<'a>(
        &'a self,
        _principal: &'a PrincipalPolicy,
        request_id: String,
        _payload: TaskIdPayload,
        _artifacts: &'a taskcaged::remote_artifact::RemoteArtifactStore,
    ) -> RemoteTaskFuture<'a> {
        self.get(_principal, request_id, _payload, _artifacts)
    }

    fn is_retained<'a>(
        &'a self,
        _principal: &'a str,
        _task_id: &'a str,
        _artifacts: &'a taskcaged::remote_artifact::RemoteArtifactStore,
    ) -> RemoteBoolFuture<'a> {
        Box::pin(async { false })
    }
}

#[tokio::test]
#[cfg(target_os = "linux")]
async fn real_tls13_listener_requires_authentication_then_serves_capabilities() {
    use rustls::pki_types::{CertificateDer, ServerName, pem::PemObject};
    use taskcaged::codec::{read_json_frame, write_json_frame};
    use taskcaged::remote_config::{REMOTE_ALPN, RemoteDaemonConfig};
    use taskcaged::remote_server::serve_remote_listener_until;
    use tokio::net::TcpListener;

    let temporary = tempfile::tempdir().expect("temporary directory");
    let certificate_path = temporary.path().join("certificate.pem");
    let key_path = temporary.path().join("private-key.pem");
    let artifact_root = temporary.path().join("artifacts");
    std::fs::create_dir(&artifact_root).expect("artifact root");
    std::fs::write(&certificate_path, include_bytes!("data/remote-chain.pem"))
        .expect("certificate PEM");
    std::fs::write(&key_path, include_bytes!("data/remote-end.key")).expect("key PEM");
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&artifact_root, std::fs::Permissions::from_mode(0o700))
        .expect("protect artifact root");
    std::fs::set_permissions(&key_path, std::fs::Permissions::from_mode(0o600))
        .expect("protect test key");

    let principal = policy("document-worker", "fixture-secret-only");
    let config_path = temporary.path().join("remote.json");
    let config_json = serde_json::json!({
        "listenAddress": "127.0.0.1:0",
        "tls": {
            "certificateChainPath": certificate_path,
            "privateKeyPath": key_path
        },
        "maxRemoteConnections": 4,
        "tlsHandshakeTimeoutMs": 2000,
        "authenticationTimeoutMs": 2000,
        "idleConnectionTimeoutMs": 1000,
        "sessionLifetimeSeconds": 60,
        "artifactRoot": artifact_root,
        "maxArtifactBytes": 1000000,
        "maxArtifactChunkBytes": 780000,
        "artifactRetentionSeconds": 600,
        "principals": [{
            "clientId": "document-worker",
            "secretVerifier": principal.secret_verifier,
            "allowedProfiles": [{"name": "ffmpeg-audio-to-wav", "version": "1.0.0"}],
            "maximumResourceOverrides": {
                "limits": {
                    "cpuMax": {"quotaMicros": 100000, "periodMicros": 100000},
                    "memoryMaxBytes": 536870912,
                    "pidsMax": 32,
                    "wallTimeLimitMs": 300000
                },
                "output": {"stdoutTailMaxBytes": 65536, "stderrTailMaxBytes": 65536}
            },
            "artifactUploadAllowed": true,
            "maxPrincipalArtifactBytes": 1000000,
            "maxPrincipalArtifacts": 4
        }]
    });
    std::fs::write(
        &config_path,
        serde_json::to_vec(&config_json).expect("config JSON"),
    )
    .expect("config file");
    std::fs::set_permissions(&config_path, std::fs::Permissions::from_mode(0o600))
        .expect("protect test config");
    let config = Arc::new(RemoteDaemonConfig::load(&config_path).expect("Remote config"));
    let credentials = CredentialStore::new(config.principals.clone());
    let artifacts = taskcaged::remote_artifact::RemoteArtifactStore::open(
        &config.artifact_root,
        config.max_artifact_bytes.get(),
        config.max_artifact_chunk_bytes.get(),
        config.artifact_retention,
    )
    .expect("Remote Artifact store");
    let backend = Arc::new(RejectingBackend {
        submit_started: tokio::sync::Notify::new(),
        submit_release: tokio::sync::Notify::new(),
        submit_completed: AtomicBool::new(false),
    });
    let dispatcher = Arc::new(taskcaged::remote_dispatch::RemoteDispatcher::new(
        artifacts,
        Arc::clone(&backend),
    ));
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("listener");
    let address = listener.local_addr().expect("listener address");
    let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel::<()>();
    let server_config = Arc::clone(&config);
    let server_credentials = credentials.clone();
    let server = tokio::spawn(async move {
        serve_remote_listener_until(
            listener,
            server_config,
            server_credentials,
            dispatcher,
            async move {
                let _ = shutdown_rx.await;
            },
        )
        .await
    });

    let mut roots = rustls::RootCertStore::empty();
    for root in CertificateDer::pem_slice_iter(include_bytes!("data/remote-root.pem")) {
        roots.add(root.expect("test root PEM")).expect("test root");
    }
    let chain = CertificateDer::pem_slice_iter(include_bytes!("data/remote-chain.pem"))
        .collect::<Result<Vec<_>, _>>()
        .expect("test chain");
    assert_eq!(config.certificate_chain(), chain.as_slice());
    roots
        .add(chain[1].clone())
        .expect("explicit intermediate trust");
    let verifier = rustls::client::WebPkiServerVerifier::builder(Arc::new(roots.clone()))
        .build()
        .expect("test verifier");
    rustls::client::danger::ServerCertVerifier::verify_server_cert(
        verifier.as_ref(),
        &chain[0],
        &chain[1..],
        &ServerName::try_from("foobar.com").expect("server name"),
        &[],
        rustls::pki_types::UnixTime::now(),
    )
    .expect("test chain and hostname verification");
    let trusted_roots = roots.clone();
    let mut tls = connect_test_tls(address, roots, "foobar.com", vec![REMOTE_ALPN.to_vec()])
        .await
        .expect("TLS connect");

    let authenticate: RemoteRequest = serde_json::from_slice(
        &std::fs::read("../protocol-fixtures/remote-v1/authenticate-request.json")
            .expect("authenticate fixture"),
    )
    .expect("authenticate request");
    write_json_frame(&mut tls, &authenticate)
        .await
        .expect("authenticate write");
    let authenticated: RemoteResponse = read_json_frame(&mut tls)
        .await
        .expect("authenticated response");
    assert!(matches!(
        authenticated,
        RemoteResponse::Authenticated { .. }
    ));

    let capabilities: RemoteRequest = serde_json::from_slice(
        &std::fs::read("../protocol-fixtures/remote-v1/get-capabilities.json")
            .expect("capabilities fixture"),
    )
    .expect("capabilities request");
    write_json_frame(&mut tls, &capabilities)
        .await
        .expect("capabilities write");
    let response: RemoteResponse = read_json_frame(&mut tls)
        .await
        .expect("capabilities response");
    assert!(matches!(response, RemoteResponse::Capabilities { .. }));

    let bytes = b"real TLS managed transfer";
    let digest = format!("sha256:{:x}", sha2::Sha256::digest(bytes));
    let begin = RemoteRequest::BeginArtifactUpload {
        remote_protocol_version: 1,
        request_id: "33333333-3333-4333-8333-333333333333".to_owned(),
        payload: BeginArtifactUploadPayload {
            client_artifact_id: "44444444-4444-4444-8444-444444444444".to_owned(),
            digest: digest.clone(),
            size_bytes: bytes.len() as u64,
            media_type: Some("application/octet-stream".to_owned()),
        },
    };
    write_json_frame(&mut tls, &begin)
        .await
        .expect("begin write");
    let started: RemoteResponse = read_json_frame(&mut tls).await.expect("begin response");
    let artifact_id = match started {
        RemoteResponse::ArtifactUploadStarted { payload, .. } => payload.artifact_id,
        other => panic!("unexpected begin response: {other:?}"),
    };
    let chunk = RemoteRequest::UploadArtifactChunk {
        remote_protocol_version: 1,
        request_id: "66666666-6666-4666-8666-666666666666".to_owned(),
        payload: UploadArtifactChunkPayload {
            artifact_id: artifact_id.clone(),
            offset: 0,
            data_base64: base64::Engine::encode(&base64::engine::general_purpose::STANDARD, bytes),
        },
    };
    write_json_frame(&mut tls, &chunk)
        .await
        .expect("chunk write");
    let accepted: RemoteResponse = read_json_frame(&mut tls).await.expect("chunk response");
    assert!(matches!(
        accepted,
        RemoteResponse::ArtifactChunkAccepted { .. }
    ));
    let complete = RemoteRequest::CompleteArtifactUpload {
        remote_protocol_version: 1,
        request_id: "77777777-7777-4777-8777-777777777777".to_owned(),
        payload: taskcaged::remote_protocol::ArtifactIdPayload { artifact_id },
    };
    write_json_frame(&mut tls, &complete)
        .await
        .expect("complete write");
    let uploaded: RemoteResponse = read_json_frame(&mut tls).await.expect("complete response");
    assert!(matches!(uploaded, RemoteResponse::ArtifactUploaded { .. }));

    let submit = RemoteRequest::SubmitProfile {
        remote_protocol_version: 1,
        request_id: "12121212-1212-4212-8212-121212121212".to_owned(),
        payload: RemoteProfileRequestPayload {
            client_request_id: "13131313-1313-4313-8313-131313131313".to_owned(),
            profile: taskcaged::remote_protocol::ProfileIdentity {
                name: "ffmpeg-audio-to-wav".to_owned(),
                version: "1.0.0".to_owned(),
            },
            inputs: BTreeMap::new(),
            resource_overrides: None,
        },
    };
    write_json_frame(&mut tls, &submit)
        .await
        .expect("slow submit write");
    tokio::time::timeout(
        std::time::Duration::from_secs(1),
        backend.submit_started.notified(),
    )
    .await
    .expect("submit dispatch started");
    assert!(
        tokio::time::timeout(
            std::time::Duration::from_secs(3),
            read_json_frame::<_, RemoteResponse>(&mut tls),
        )
        .await
        .expect("idle timeout closes connection during request processing")
        .is_err()
    );
    backend.submit_release.notify_one();
    tokio::time::timeout(std::time::Duration::from_secs(1), async {
        while !backend.submit_completed.load(Ordering::SeqCst) {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("idle timeout must not cancel the accepted operation");
    drop(tls);

    backend.submit_completed.store(false, Ordering::SeqCst);
    let mut revoked_tls = connect_test_tls(
        address,
        trusted_roots.clone(),
        "foobar.com",
        vec![REMOTE_ALPN.to_vec()],
    )
    .await
    .expect("revocation TLS connect");
    write_json_frame(&mut revoked_tls, &authenticate)
        .await
        .expect("revocation authenticate write");
    let authenticated: RemoteResponse = read_json_frame(&mut revoked_tls)
        .await
        .expect("revocation authenticated response");
    assert!(matches!(
        authenticated,
        RemoteResponse::Authenticated { .. }
    ));
    let revoked_submit = RemoteRequest::SubmitProfile {
        remote_protocol_version: 1,
        request_id: "14141414-1414-4414-8414-141414141414".to_owned(),
        payload: RemoteProfileRequestPayload {
            client_request_id: "15151515-1515-4515-8515-151515151515".to_owned(),
            profile: taskcaged::remote_protocol::ProfileIdentity {
                name: "ffmpeg-audio-to-wav".to_owned(),
                version: "1.0.0".to_owned(),
            },
            inputs: BTreeMap::new(),
            resource_overrides: None,
        },
    };
    write_json_frame(&mut revoked_tls, &revoked_submit)
        .await
        .expect("revoked submit write");
    tokio::time::timeout(
        std::time::Duration::from_secs(1),
        backend.submit_started.notified(),
    )
    .await
    .expect("revoked submit dispatch started");
    credentials.revoke("document-worker");
    assert!(
        tokio::time::timeout(
            std::time::Duration::from_secs(1),
            read_json_frame::<_, RemoteResponse>(&mut revoked_tls),
        )
        .await
        .expect("revocation closes connection")
        .is_err()
    );
    backend.submit_release.notify_one();
    tokio::time::timeout(std::time::Duration::from_secs(1), async {
        while !backend.submit_completed.load(Ordering::SeqCst) {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("revoked connection must not cancel the accepted operation");
    drop(revoked_tls);

    assert!(
        connect_test_tls(
            address,
            rustls::RootCertStore::empty(),
            "foobar.com",
            vec![REMOTE_ALPN.to_vec()],
        )
        .await
        .is_err(),
        "untrusted certificate must fail"
    );
    assert!(
        connect_test_tls(
            address,
            trusted_roots.clone(),
            "example.invalid",
            vec![REMOTE_ALPN.to_vec()],
        )
        .await
        .is_err(),
        "hostname mismatch must fail"
    );
    assert!(
        connect_test_tls(
            address,
            trusted_roots.clone(),
            "foobar.com",
            vec![b"not-taskcage".to_vec()],
        )
        .await
        .is_err(),
        "ALPN mismatch must fail"
    );

    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let mut plaintext = tokio::net::TcpStream::connect(address)
        .await
        .expect("plaintext TCP connect");
    plaintext
        .write_all(&[0, 0, 0, 2, b'{', b'}'])
        .await
        .expect("plaintext write");
    let mut response_byte = [0_u8; 1];
    match tokio::time::timeout(
        std::time::Duration::from_secs(1),
        plaintext.read(&mut response_byte),
    )
    .await
    {
        Ok(Ok(0)) | Ok(Err(_)) => {}
        Ok(Ok(1)) => assert_eq!(
            response_byte[0], 21,
            "plaintext rejection may return only a TLS alert record"
        ),
        other => panic!("plaintext must close without application bytes: {other:?}"),
    }

    let mut unauthenticated = connect_test_tls(
        address,
        trusted_roots.clone(),
        "foobar.com",
        vec![REMOTE_ALPN.to_vec()],
    )
    .await
    .expect("pre-auth TLS connect");
    write_json_frame(&mut unauthenticated, &capabilities)
        .await
        .expect("pre-auth operation write");
    let required: RemoteResponse = read_json_frame(&mut unauthenticated)
        .await
        .expect("authentication required response");
    assert!(matches!(
        required,
        RemoteResponse::Error {
            payload: taskcaged::remote_protocol::ErrorPayload {
                code: RemoteErrorCode::AuthenticationRequired,
                ..
            },
            ..
        }
    ));
    assert!(
        read_json_frame::<_, RemoteResponse>(&mut unauthenticated)
            .await
            .is_err()
    );

    let unknown = authentication_failure(
        address,
        trusted_roots.clone(),
        "unknown-worker",
        "fixture-secret-only",
        "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa",
    )
    .await;
    let wrong = authentication_failure(
        address,
        trusted_roots,
        "document-worker",
        "wrong-secret",
        "bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb",
    )
    .await;
    assert_eq!(unknown, wrong);

    let _ = shutdown_tx.send(());
    server.await.expect("server task").expect("server shutdown");
}

#[cfg(target_os = "linux")]
async fn connect_test_tls(
    address: std::net::SocketAddr,
    roots: rustls::RootCertStore,
    server_name: &str,
    alpn: Vec<Vec<u8>>,
) -> std::io::Result<tokio_rustls::client::TlsStream<tokio::net::TcpStream>> {
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let mut client_config = rustls::ClientConfig::builder_with_provider(provider)
        .with_protocol_versions(&[&rustls::version::TLS13])
        .expect("TLS 1.3")
        .with_root_certificates(roots)
        .with_no_client_auth();
    client_config.alpn_protocols = alpn;
    let tcp = tokio::net::TcpStream::connect(address).await?;
    tokio_rustls::TlsConnector::from(Arc::new(client_config))
        .connect(
            rustls::pki_types::ServerName::try_from(server_name.to_owned())
                .expect("test server name"),
            tcp,
        )
        .await
}

#[cfg(target_os = "linux")]
async fn authentication_failure(
    address: std::net::SocketAddr,
    roots: rustls::RootCertStore,
    client_id: &str,
    secret: &str,
    request_id: &str,
) -> (RemoteErrorCode, bool, String) {
    use taskcaged::codec::{read_json_frame, write_json_frame};
    use taskcaged::remote_config::REMOTE_ALPN;
    use taskcaged::remote_protocol::{AuthenticatePayload, RemoteRequest, RemoteResponse};

    let mut tls = connect_test_tls(address, roots, "foobar.com", vec![REMOTE_ALPN.to_vec()])
        .await
        .expect("authentication failure TLS connect");
    let request = RemoteRequest::Authenticate {
        remote_protocol_version: 1,
        request_id: request_id.to_owned(),
        payload: AuthenticatePayload {
            client_id: client_id.to_owned(),
            secret: secret.to_owned(),
        },
    };
    write_json_frame(&mut tls, &request)
        .await
        .expect("failed authentication write");
    match read_json_frame::<_, RemoteResponse>(&mut tls)
        .await
        .expect("failed authentication response")
    {
        RemoteResponse::Error { payload, .. } => {
            assert_eq!(payload.code, RemoteErrorCode::AuthenticationFailed);
            (payload.code, payload.retryable, payload.message)
        }
        other => panic!("unexpected failed authentication response: {other:?}"),
    }
}
