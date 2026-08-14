# Remote daemon 실행

Remote Protocol v1은 기존 Local UDS listener를 유지하면서 별도 TLS 1.3 TCP listener를 연다. 승인된 wire 계약은
`docs/remote-protocol-v1.md`와 `protocol-fixtures/remote-v1/`이며 Remote Raw Command는 항상 거부한다.

`serve`의 기존 필수 옵션과 Profile 설정에 `--remote-config /absolute/path/remote.json`을 추가한다. config와
TLS private key는 service UID가 소유한 mode `0600` regular file이어야 하고, `artifactRoot`는 같은 UID가 소유한
mode `0700` directory여야 한다. secret과 private key는 인자, URI, log에 넣지 않는다.

```json
{
  "listenAddress": "0.0.0.0:7443",
  "tls": {
    "certificateChainPath": "/etc/taskcage/tls/chain.pem",
    "privateKeyPath": "/etc/taskcage/tls/private-key.pem"
  },
  "maxRemoteConnections": 64,
  "tlsHandshakeTimeoutMs": 3000,
  "authenticationTimeoutMs": 3000,
  "idleConnectionTimeoutMs": 30000,
  "sessionLifetimeSeconds": 1800,
  "artifactRoot": "/var/lib/taskcage/remote-artifacts",
  "maxArtifactBytes": 104857600,
  "maxArtifactChunkBytes": 780000,
  "artifactRetentionSeconds": 600,
  "principals": [{
    "clientId": "document-worker",
    "secretVerifier": "$argon2id$v=19$m=19456,t=2,p=1$...",
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
    "maxPrincipalArtifactBytes": 209715200,
    "maxPrincipalArtifacts": 8
  }]
}
```

secret verifier는 secret bytes를 stdin으로만 전달해 만든다. shell history와 process argv에 secret을 넣지 않는다.

```sh
printf %s "$TASKCAGE_CLIENT_SECRET" | taskcaged hash-remote-secret
```

principal secret/policy를 회전한 뒤 config를 원자적으로 교체하고 daemon에 `SIGHUP`을 보낸다. 새 연결은 새
verifier를 사용한다. config에서 제거한 principal의 현재 session은 즉시 닫히며 기존 Task는 취소하지 않는다.
TLS certificate 교체는 daemon 재시작이 필요하다.
