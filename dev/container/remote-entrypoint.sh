#!/bin/sh
set -eu

mkdir -p /run/taskcage /taskcage-work /taskcage-work/artifacts /etc/taskcage/tls
chmod 0700 /run/taskcage /taskcage-work /taskcage-work/artifacts /etc/taskcage /etc/taskcage/tls
cp /usr/local/share/taskcage/remote-chain.pem /etc/taskcage/tls/chain.pem
cp /usr/local/share/taskcage/remote-end.key /etc/taskcage/tls/private-key.pem
chmod 0600 /etc/taskcage/tls/private-key.pem
verifier="$(printf %s fixture-secret-only | /usr/local/bin/taskcaged hash-remote-secret)"
cat > /etc/taskcage/remote.json <<EOF
{"listenAddress":"0.0.0.0:7443","tls":{"certificateChainPath":"/etc/taskcage/tls/chain.pem","privateKeyPath":"/etc/taskcage/tls/private-key.pem"},"maxRemoteConnections":8,"tlsHandshakeTimeoutMs":3000,"authenticationTimeoutMs":3000,"idleConnectionTimeoutMs":30000,"sessionLifetimeSeconds":1800,"artifactRoot":"/taskcage-work/artifacts","maxArtifactBytes":10485760,"maxArtifactChunkBytes":65536,"artifactRetentionSeconds":600,"principals":[{"clientId":"document-worker","secretVerifier":"$verifier","allowedProfiles":[{"name":"file-copy","version":"1.0.0"},{"name":"ffmpeg-audio-to-wav","version":"1.0.0"}],"maximumResourceOverrides":{"limits":{"cpuMax":{"quotaMicros":100000,"periodMicros":100000},"memoryMaxBytes":536870912,"pidsMax":32,"wallTimeLimitMs":300000},"output":{"stdoutTailMaxBytes":65536,"stderrTailMaxBytes":65536}},"artifactUploadAllowed":true,"maxPrincipalArtifactBytes":10485760,"maxPrincipalArtifacts":8}]}
EOF
chmod 0600 /etc/taskcage/remote.json
if [ "$#" -eq 0 ]; then
  set -- serve --socket /run/taskcage/taskcaged.sock --max-concurrent-tasks 4 --max-registry-tasks 1000 \
    --max-concurrent-connections 32 --cleanup-timeout-ms 5000 --fail-stop-timeout-ms 10000 \
    --max-task-cpu-quota-us 200000 --max-task-cpu-period-us 100000 --max-task-memory-bytes 2147483648 \
    --max-task-pids 128 --max-task-timeout-ms 900000 --max-task-stdout-tail-bytes 65536 \
    --max-task-stderr-tail-bytes 65536 --profile-artifact-root /taskcage-work/artifacts \
    --profile-artifact-max-bytes 104857600
fi
exec /usr/local/bin/taskcage-container-entrypoint "$@" --remote-config /etc/taskcage/remote.json
