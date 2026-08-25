#!/bin/sh
set -eu

work_root=/taskcage-work
cache_root=${work_root}/runtime-package-cache

case "$(uname -m)" in
  x86_64) platform=linux/amd64 ;;
  aarch64) platform=linux/arm64 ;;
  *)
    echo "ERROR: benchmark Capsules require Linux x86_64 or aarch64" >&2
    exit 1
    ;;
esac

build_runtime() {
  name=$1
  executable=$2
  source_root=${work_root}/${name}-rootfs
  runtime_package=${work_root}/${name}-runtime-package

  rm -rf -- "${source_root}" "${runtime_package}"
  mkdir -p "${source_root}/bin" "${source_root}/lib" "${source_root}/share"
  chmod 0700 "${source_root}"
  cp "${executable}" "${source_root}/bin/${name}"
  chmod 0555 "${source_root}/bin/${name}"

  ldd "${executable}" \
    | awk '/=> \/[^ ]+/ { print $3 } /^[[:space:]]*\// { print $1 }' \
    | sort -u \
    | while IFS= read -r library; do
        case "${library}" in
          */ld-linux-*.so.*|*/libc.so.6|*/libm.so.6|*/libpthread.so.0|*/librt.so.1|*/libdl.so.2)
            continue
            ;;
        esac
        cp --dereference "${library}" "${source_root}/lib/$(basename "${library}")"
        chmod 0444 "${source_root}/lib/$(basename "${library}")"
      done

  printf '%s\n' '{"spdxVersion":"SPDX-2.3"}' >"${source_root}/share/sbom.spdx.json"
  chmod 0444 "${source_root}/share/sbom.spdx.json"

  taskcage runtime build \
    --source-rootfs "${source_root}" \
    --output "${runtime_package}" \
    --id "org.taskcage.${name}" \
    --version 0.0.0-container.1 \
    --platform "${platform}" \
    --glibc-minimum 2.17 \
    --entrypoint "bin/${name}" \
    --library-path lib \
    --sbom share/sbom.spdx.json >/dev/null
}

install_capsule() {
  name=$1
  capsulefile=$2
  runtime_package=${work_root}/${name}-runtime-package
  pack=${work_root}/${name}-1.0.0.tccapsule

  rm -f -- "${pack}"
  taskcage capsule build "${capsulefile}" \
    --runtime-package "${runtime_package}" \
    --platform "${platform}" \
    --output "${pack}" >/dev/null
  taskcage capsule install "${pack}" --cache-root "${cache_root}" >/dev/null
}

mkdir -p "${cache_root}"
chmod 0700 "${cache_root}"

build_runtime ghost-tree /usr/local/libexec/taskcage/ghost-tree
install_capsule ghost-tree /usr/local/share/taskcage/ghost-tree-timeout.Capsulefile

build_runtime memory-hog /usr/local/libexec/taskcage/memory-hog
install_capsule memory-hog /usr/local/share/taskcage/memory-hog-limit.Capsulefile
