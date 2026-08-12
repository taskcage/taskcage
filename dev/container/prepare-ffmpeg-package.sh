#!/bin/sh
set -eu

source_root=/taskcage-work/runtime-package-source
cache_root=/taskcage-work/runtime-package-cache
entrypoint=${source_root}/rootfs/bin/ffmpeg
sbom=${source_root}/rootfs/share/sbom.spdx.json
manifest=${source_root}/runtime-package.json

rm -rf -- "${source_root}"
mkdir -p "${source_root}/rootfs/bin" "${source_root}/rootfs/share"
mkdir -p "${cache_root}"
chmod 0700 "${source_root}" "${cache_root}"

cp /usr/bin/ffmpeg "${entrypoint}"
chmod 0555 "${entrypoint}"
printf '%s\n' '{"spdxVersion":"SPDX-2.3"}' >"${sbom}"
chmod 0444 "${sbom}"

entrypoint_digest="$(sha256sum "${entrypoint}" | cut -d ' ' -f 1)"
entrypoint_size="$(stat -c '%s' "${entrypoint}")"
sbom_digest="$(sha256sum "${sbom}" | cut -d ' ' -f 1)"
sbom_size="$(stat -c '%s' "${sbom}")"

printf '%s\n' \
  '{' \
  '  "schemaVersion": "taskcage.runtime-package/v0alpha1",' \
  '  "id": "org.taskcage.ffmpeg",' \
  '  "version": "0.0.0-container.1",' \
  '  "platform": {' \
  '    "os": "linux",' \
  '    "architecture": "x86_64",' \
  '    "abi": "gnu",' \
  '    "libc": { "family": "glibc", "minimumVersion": "2.17" }' \
  '  },' \
  '  "entrypoint": "bin/ffmpeg",' \
  '  "libraryPaths": [],' \
  '  "files": [' \
  '    {' \
  '      "path": "bin/ffmpeg",' \
  "      \"digest\": \"sha256:${entrypoint_digest}\"," \
  "      \"sizeBytes\": ${entrypoint_size}," \
  '      "mode": "0555"' \
  '    },' \
  '    {' \
  '      "path": "share/sbom.spdx.json",' \
  "      \"digest\": \"sha256:${sbom_digest}\"," \
  "      \"sizeBytes\": ${sbom_size}," \
  '      "mode": "0444"' \
  '    }' \
  '  ],' \
  '  "licenses": [],' \
  '  "sbom": { "format": "SPDX-JSON-2.3", "path": "share/sbom.spdx.json" }' \
  '}' >"${manifest}"
chmod 0444 "${manifest}"

report="$(taskcaged import-package --source "${source_root}" --cache-root "${cache_root}")"
digest="$(printf '%s\n' "${report}" | sed -n 's/.*"digest":"\([^"]*\)".*/\1/p')"
case "${digest}" in
  sha256:????????????????????????????????????????????????????????????????) ;;
  *)
    echo "FAIL: Runtime Package import did not return a canonical digest" >&2
    exit 1
    ;;
esac

printf '%s\n' "${digest}"
