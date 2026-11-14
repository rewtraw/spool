#!/bin/sh
# Update Spool's helper on a TrueNAS SCALE machine, over SSH: build its image here, hand it
# to the machine, and restart the helper on it.
#
# Settings come from the environment or from deploy/local.env (not tracked):
#
#   SPOOL_HELPER_HOST   SSH host the helper runs on, as root or an admin (required)
#   SPOOL_HELPER_URL    where the helper answers, such as http://nas:7978 (required)
#   SPOOL_HELPER_KEY    the helper's key (required)
#   SPOOL_HELPER_APP    the app's name on TrueNAS (default: spool-helper)
#   SPOOL_FORCE         set to restart even though the helper says it is in the middle of something
#
# The binary is built here for x86-64 Linux with cargo-zigbuild, which keeps its build between
# runs, so only what changed is compiled again. It goes into the image the Dockerfile makes,
# tagged spool:local, which the app on TrueNAS runs; the layers under it are already there, so
# little more than the binary crosses the network. Needs zig, cargo-zigbuild, the
# x86_64-unknown-linux-gnu Rust target and Docker.
#
# The helper is asked first whether a restart would interrupt anything: a download being
# unpacked, a file being filed, a title being moved. Downloads still coming down are
# checkpointed and carry on.
set -eu
cd "$(dirname "$0")/.."
[ -f deploy/local.env ] && . deploy/local.env
HOST="${SPOOL_HELPER_HOST:?set SPOOL_HELPER_HOST, or put it in deploy/local.env}"
URL="${SPOOL_HELPER_URL:?set SPOOL_HELPER_URL}"
KEY="${SPOOL_HELPER_KEY:?set SPOOL_HELPER_KEY}"
APP="${SPOOL_HELPER_APP:-spool-helper}"
ask() { curl -fsS -m 8 -H "Authorization: Bearer $KEY" "$@"; }
SOURCE=https://github.com/rewtraw/spool

# The web app is built into the binary; deploy.sh has usually just built it.
[ -f web/dist/index.html ] || (cd web && bun install --frozen-lockfile >/dev/null && bun run build >/dev/null)
# Debian bookworm's C library, which the image is built on.
cargo zigbuild --release --locked -p spool --target x86_64-unknown-linux-gnu.2.36
docker buildx build --quiet --platform linux/amd64 --load \
  --build-arg SPOOL_BINARY=prebuilt --build-context binary=target/x86_64-unknown-linux-gnu/release \
  --label "org.opencontainers.image.source=$SOURCE" --label "org.opencontainers.image.revision=$(git rev-parse HEAD)" \
  -t spool:local . >/dev/null

if INFO=$(ask "$URL/link/work/v1/info" 2>/dev/null); then
  case "$INFO" in
    *'"busy":[]'*) ;;
    *)
      echo "The helper on $HOST is in the middle of something:"
      printf '%s\n' "$INFO" | sed -e 's/.*"busy":\[//' -e 's/\].*//' -e 's/","/\n/g' -e 's/"//g' | sed 's/^/  - /'
      if [ -z "${SPOOL_FORCE:-}" ]; then
        echo "Not restarting it. Try again when that is done, or set SPOOL_FORCE=1 to restart anyway."
        exit 1
      fi
      ;;
  esac
fi

docker save spool:local | gzip -1 | ssh "$HOST" 'gunzip | docker load' >/dev/null
# Its progress bars say nothing a failure would not; a failure still stops the script.
ssh "$HOST" "midclt call --job app.redeploy '$APP'" >/dev/null 2>&1
# Earlier images of Spool, left without a name by the one just loaded. Nothing else is touched.
ssh "$HOST" "docker image prune -f --filter 'label=org.opencontainers.image.source=$SOURCE'" >/dev/null
for i in $(seq 1 30); do
  if INFO=$(ask "$URL/link/work/v1/info" 2>/dev/null); then
    echo "The helper on $HOST is updated: $(printf '%s' "$INFO" | sed -e 's/.*"version":"\([^"]*\)".*/version \1/')"
    exit 0
  fi
  sleep 2
done
echo "The helper on $HOST did not answer after the update."
exit 1
