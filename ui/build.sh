#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
WWWROOT="${SCRIPT_DIR}/../wwwroot"
DX_OUT="${SCRIPT_DIR}/target/dx/my-no-sql-grpc-ui/release/web/public"

cd "${SCRIPT_DIR}"

echo ">> dx build --release --web"
dx build --release --web

if [ ! -d "${DX_OUT}" ]; then
    echo "ERROR: build output not found at ${DX_OUT}"
    exit 1
fi

echo ">> cleaning ${WWWROOT}"
rm -rf "${WWWROOT}"
mkdir -p "${WWWROOT}"

echo ">> copying ${DX_OUT}/. -> ${WWWROOT}/"
cp -R "${DX_OUT}/." "${WWWROOT}/"

echo ">> done."
