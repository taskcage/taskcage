#!/bin/sh
set -eu

source_root=/taskcage-work/ffmpeg-rootfs
runtime_package=/taskcage-work/runtime-package-source
entrypoint=${source_root}/bin/ffmpeg
library_root=${source_root}/lib
sbom=${source_root}/share/sbom.spdx.json

case "$(uname -m)" in
  x86_64|aarch64) package_architecture="$(uname -m)" ;;
  *)
    echo "ERROR: Runtime Package requires Linux x86_64 or aarch64" >&2
    exit 1
    ;;
esac

rm -rf -- "${source_root}" "${runtime_package}"
mkdir -p "${source_root}/bin" "${library_root}" "${source_root}/share"
chmod 0700 "${source_root}"

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

case "${package_architecture}" in
  x86_64) platform=linux/amd64 ;;
  aarch64) platform=linux/arm64 ;;
esac

taskcage runtime build \
  --source-rootfs "${source_root}" \
  --output "${runtime_package}" \
  --id org.taskcage.ffmpeg \
  --version 0.0.0-container.1 \
  --platform "${platform}" \
  --glibc-minimum 2.17 \
  --entrypoint bin/ffmpeg \
  --library-path lib \
  --sbom share/sbom.spdx.json >/dev/null
