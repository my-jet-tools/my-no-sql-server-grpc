#!/usr/bin/env bash
# Builds the Dioxus UI (in ui/) and copies it into wwwroot/, which is committed
# and baked into the docker image by the Dockerfile. Run this after changing
# anything under ui/, then commit the updated wwwroot/.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

exec "${SCRIPT_DIR}/ui/build.sh"
