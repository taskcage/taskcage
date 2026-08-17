#!/bin/sh
set -eu

runtime_digest=${1:?Runtime Package digest is required}
source_root=/taskcage-work/bundle-source
cache_root=/taskcage-work/runtime-package-cache
archive=/taskcage-work/ffmpeg-audio-to-wav-1.0.0.tcbundle.tar.gz
key=/taskcage-work/bundle-signing-key.pem
public_key=/taskcage-work/bundle-signing-key.pub

case "${runtime_digest}" in
  sha256:????????????????????????????????????????????????????????????????) ;;
  *)
    echo "ERROR: Runtime Package digest must be canonical SHA-256" >&2
    exit 1
    ;;
esac

rm -rf -- "${source_root}" "${archive}" "${key}" "${public_key}"
mkdir -p "${source_root}"
chmod 0700 "${source_root}"

profile="${source_root}/profile.json"
bundle="${source_root}/bundle.json"
checksums="${source_root}/checksums.txt"
signature="${source_root}/signature.sig"

printf '%s\n' \
  '{' \
  '  "schemaVersion": "taskcage.profile/v0alpha1",' \
  '  "name": "ffmpeg-audio-to-wav",' \
  '  "version": "1.0.0",' \
  '  "inputs": [' \
  '    {"name":"source","kind":"LOCAL_INPUT","required":true},' \
  '    {"name":"sample_rate_hz","kind":"INT64","required":true,"minimum":8000,"maximum":48000},' \
  '    {"name":"channels","kind":"INT64","required":true,"minimum":1,"maximum":2}' \
  '  ],' \
  '  "output": {"name":"audio","fileName":"result.wav","mediaType":"audio/wav","maximumBytes":104857600},' \
  '  "argv": [' \
  '    "-hide_banner", "-loglevel", "error", "-nostdin", "-i", {"input":"source"},' \
  '    "-map", "0:a:0", "-vn", "-c:a", "pcm_s16le", "-ar", {"int64":"sample_rate_hz"},' \
  '    "-ac", {"int64":"channels"}, {"output":"audio"}' \
  '  ],' \
  '  "policy": {' \
  '    "limits": {"cpuMax":{"quotaMicros":100000,"periodMicros":100000},"memoryMaxBytes":536870912,"pidsMax":32,"wallTimeLimitMs":120000},' \
  '    "output": {"stdoutTailMaxBytes":65536,"stderrTailMaxBytes":65536}' \
  '  },' \
  '  "allowedOverrides": []' \
  '}' >"${profile}"

profile_digest="$(sha256sum "${profile}" | awk '{print $1}')"
openssl genpkey -algorithm ED25519 -out "${key}" >/dev/null 2>&1
openssl pkey -in "${key}" -pubout -outform DER \
  | tail -c 32 | base64 -w 0 | tr -d '=' >"${public_key}"
chmod 0600 "${key}"
chmod 0444 "${public_key}"

printf '%s\n' \
  '{' \
  '  "schemaVersion": "taskcage.bundle/v0alpha1",' \
  '  "name": "ffmpeg-audio-to-wav",' \
  '  "version": "1.0.0",' \
  '  "signingKeyId": "container-test",' \
  '  "runtime": {' \
  '    "packageId": "org.taskcage.ffmpeg",' \
  "    \"digest\": \"${runtime_digest}\"" \
  '  },' \
  "  \"profileDigest\": \"sha256:${profile_digest}\"" \
  '}' >"${bundle}"

printf '%s  bundle.json\n%s  profile.json\n' \
  "$(sha256sum "${bundle}" | awk '{print $1}')" \
  "${profile_digest}" >"${checksums}"
openssl pkeyutl -sign -inkey "${key}" -rawin -in "${checksums}" \
  | base64 -w 0 | tr -d '=' >"${signature}"
chmod 0444 "${bundle}" "${profile}" "${checksums}" "${signature}"

tar -C "${source_root}" -czf "${archive}" \
  bundle.json profile.json checksums.txt signature.sig
chmod 0444 "${archive}"

taskcaged bundle import \
  --source "${archive}" \
  --cache-root "${cache_root}" \
  --trusted-key "container-test=${public_key}" >/dev/null
