#!/bin/sh
set -eu

source_root=/taskcage-work/runtime-package-source
cache_root=/taskcage-work/runtime-package-cache
entrypoint=${source_root}/rootfs/bin/ffmpeg
library_root=${source_root}/rootfs/lib
sbom=${source_root}/rootfs/share/sbom.spdx.json
manifest=${source_root}/runtime-package.json
file_entries=${cache_root}/ffmpeg-files.json

case "$(uname -m)" in
  x86_64|aarch64) package_architecture="$(uname -m)" ;;
  *)
    echo "ERROR: Runtime Package requires Linux x86_64 or aarch64" >&2
    exit 1
    ;;
esac

rm -rf -- "${source_root}"
mkdir -p "${source_root}/rootfs/bin" "${library_root}" "${source_root}/rootfs/share"
mkdir -p "${cache_root}"
chmod 0700 "${source_root}" "${cache_root}"

cp /usr/bin/ffmpeg "${entrypoint}"
chmod 0555 "${entrypoint}"

# A Runtime Package carries non-glibc shared libraries with the executable. The container's glibc
# and dynamic loader remain the declared Linux platform dependency.
ldd /usr/bin/ffmpeg \
  | awk '/=> \/[^ ]+/ { print $3 } /^[[:space:]]*\// { print $1 }' \
  | sort -u \
  | while IFS= read -r library; do
      case "${library}" in
        */ld-linux-*.so.*|*/libc.so.6|*/libm.so.6|*/libpthread.so.0|*/librt.so.1|*/libdl.so.2)
          continue
          ;;
      esac
      cp --dereference "${library}" "${library_root}/$(basename "${library}")"
      chmod 0444 "${library_root}/$(basename "${library}")"
    done

printf '%s\n' '{"spdxVersion":"SPDX-2.3"}' >"${sbom}"
chmod 0444 "${sbom}"

{
  first=true
  find "${source_root}/rootfs" -type f -printf '%P\n' | sort | while IFS= read -r path; do
    source_file="${source_root}/rootfs/${path}"
    case "${path}" in
      bin/*) mode=0555 ;;
      *) mode=0444 ;;
    esac
    if [ "${first}" = true ]; then
      first=false
    else
      printf ',\n'
    fi
    printf '    {"path":"%s","digest":"sha256:%s","sizeBytes":%s,"mode":"%s"}' \
      "${path}" \
      "$(sha256sum "${source_file}" | cut -d ' ' -f 1)" \
      "$(stat -c '%s' "${source_file}")" \
      "${mode}"
  done
  printf '\n'
} >"${file_entries}"

printf '%s\n' \
  '{' \
  '  "schemaVersion": "taskcage.runtime-package/v0alpha1",' \
  '  "id": "org.taskcage.ffmpeg",' \
  '  "version": "0.0.0-container.1",' \
  '  "platform": {' \
  '    "os": "linux",' \
  "    \"architecture\": \"${package_architecture}\"," \
  '    "abi": "gnu",' \
  '    "libc": { "family": "glibc", "minimumVersion": "2.17" }' \
  '  },' \
  '  "entrypoint": "bin/ffmpeg",' \
  '  "libraryPaths": ["lib"],' \
  '  "files": [' \
  "$(cat "${file_entries}")" \
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
