#!/bin/sh
set -eu

mkdir -p /run/taskcage \
  /etc/taskcage/trusted-capsules.d \
  /var/lib/taskcage/artifacts \
  /var/lib/taskcage/remote-artifacts \
  /var/lib/taskcage/runtime-package-cache
chmod 0755 /etc/taskcage/trusted-capsules.d
chmod 0700 /run/taskcage /var/lib/taskcage /var/lib/taskcage/artifacts /var/lib/taskcage/remote-artifacts \
  /var/lib/taskcage/runtime-package-cache

bootstrap_default_remote() {
  remote_config=/var/lib/taskcage/config/remote.json
  config_directory=/var/lib/taskcage/config
  local_secret="${TASKCAGE_CLIENT_SECRET:-}"
  if [ -z "${local_secret}" ]; then
    echo "ERROR: TASKCAGE_CLIENT_SECRET is required when no Remote configuration is supplied" >&2
    exit 64
  fi

  temporary_directory="${config_directory}.new"
  rm -rf -- "${temporary_directory}"
  mkdir -p "${temporary_directory}/tls"
  umask 077
  openssl req -x509 -newkey rsa:2048 -nodes -sha256 -days 7 \
    -subj "/CN=taskcage" \
    -addext "subjectAltName=DNS:localhost,IP:127.0.0.1" \
    -keyout "${temporary_directory}/tls/private-key.pem" \
    -out "${temporary_directory}/tls/chain.pem" >/dev/null 2>&1
  secret_verifier="$(printf %s "${local_secret}" | taskcaged hash-remote-secret)"
  printf '%s\n' \
    '{' \
    '  "listenAddress": "0.0.0.0:7443",' \
    '  "tls": {' \
    '    "certificateChainPath": "/var/lib/taskcage/config/tls/chain.pem",' \
    '    "privateKeyPath": "/var/lib/taskcage/config/tls/private-key.pem"' \
    '  },' \
    '  "maxRemoteConnections": 4,' \
    '  "tlsHandshakeTimeoutMs": 3000,' \
    '  "authenticationTimeoutMs": 3000,' \
    '  "idleConnectionTimeoutMs": 30000,' \
    '  "sessionLifetimeSeconds": 1800,' \
    '  "artifactRoot": "/var/lib/taskcage/remote-artifacts",' \
    '  "maxArtifactBytes": 104857600,' \
    '  "maxArtifactChunkBytes": 65536,' \
    '  "artifactRetentionSeconds": 600,' \
    '  "principals": [{' \
    '    "clientId": "taskcage",' \
    "    \"secretVerifier\": \"${secret_verifier}\"," \
    '    "allowedProfiles": [],' \
    '    "allowAllInstalledCapsules": true,' \
    '    "artifactUploadAllowed": true,' \
    '    "maxPrincipalArtifactBytes": 104857600,' \
    '    "maxPrincipalArtifacts": 8' \
    '  }]' \
    '}' >"${temporary_directory}/remote.json"
  chmod 0700 "${temporary_directory}" "${temporary_directory}/tls"
  chmod 0600 "${temporary_directory}/remote.json" "${temporary_directory}/tls/chain.pem" \
    "${temporary_directory}/tls/private-key.pem"
  rm -rf -- "${config_directory}"
  mv -- "${temporary_directory}" "${config_directory}"
  echo "INFO: initialized default TLS configuration" >&2
  unset local_secret secret_verifier
}

if [ "${1:-}" = "taskcaged" ] && [ "${2:-}" = "serve" ]; then
  remote_config=/var/lib/taskcage/config/remote.json
  config_directory=/var/lib/taskcage/config
  bootstrap_config="${TASKCAGE_REMOTE_BOOTSTRAP_DIR}/remote.json"
  if [ -r "${bootstrap_config}" ]; then
    if [ ! -d "${TASKCAGE_REMOTE_BOOTSTRAP_DIR}/tls" ]; then
      echo "ERROR: TLS bootstrap requires ${TASKCAGE_REMOTE_BOOTSTRAP_DIR}/tls" >&2
      exit 64
    fi
    temporary_directory="${config_directory}.new"
    rm -rf -- "${temporary_directory}"
    mkdir -p "${temporary_directory}/tls"
    cp --dereference "${bootstrap_config}" "${temporary_directory}/remote.json"
    cp --dereference --recursive "${TASKCAGE_REMOTE_BOOTSTRAP_DIR}/tls/." "${temporary_directory}/tls/"
    chmod 0700 "${temporary_directory}" "${temporary_directory}/tls"
    chmod 0600 "${temporary_directory}/remote.json"
    find "${temporary_directory}/tls" -type f -exec chmod 0600 {} +
    rm -rf -- "${config_directory}"
    mv -- "${temporary_directory}" "${config_directory}"
  fi
  if [ ! -r "${remote_config}" ]; then
    bootstrap_default_remote
  fi
  if [ ! -r "${remote_config}" ]; then
    echo "ERROR: TLS daemon requires ${remote_config} or a bootstrap remote.json" >&2
    exit 64
  fi
  set -- "$@" --remote-config "${remote_config}"
fi

exec "$@"
