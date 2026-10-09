#!/bin/sh
# Build Spool and install it as a LaunchAgent on a Mac, over SSH, then restart it.
#
# Settings come from the environment or from deploy/local.env (not tracked):
#
#   SPOOL_HOST           SSH host to install on (required)
#   SPOOL_SIGN_IDENTITY  codesign identity; unset signs ad hoc
#   SPOOL_LABEL          LaunchAgent label and signing identifier (default: local.spool)
#   SPOOL_BIND           address to listen on (default: 0.0.0.0:7979)
#
# Signing with a stable identity matters when the library is on a removable disk: macOS ties
# the permission to read it to the program's signature, and an ad hoc build is a new program
# to macOS every time it is rebuilt.
set -eu
cd "$(dirname "$0")/.."
[ -f deploy/local.env ] && . deploy/local.env
HOST="${SPOOL_HOST:?set SPOOL_HOST, or put it in deploy/local.env}"
LABEL="${SPOOL_LABEL:-local.spool}"
BIND="${SPOOL_BIND:-0.0.0.0:7979}"

(cd web && bun install --frozen-lockfile >/dev/null && bun run build >/dev/null)
cargo build --release -p spool
codesign --force -s "${SPOOL_SIGN_IDENTITY:--}" --identifier "$LABEL" target/release/spool
codesign --verify --strict target/release/spool

REMOTE_HOME=$(ssh "$HOST" 'printf %s "$HOME"')
PLIST=$(mktemp)
trap 'rm -f "$PLIST"' EXIT
sed -e "s|@LABEL@|$LABEL|g" -e "s|@HOME@|$REMOTE_HOME|g" -e "s|@BIND@|$BIND|g" deploy/spool.plist.template > "$PLIST"

ssh "$HOST" 'mkdir -p ~/spool/bin ~/Library/LaunchAgents'
scp -q target/release/spool "$HOST:spool/bin/spool.new"
scp -q "$PLIST" "$HOST:Library/LaunchAgents/$LABEL.plist"
ssh "$HOST" LABEL="$LABEL" sh <<'REMOTE'
  set -e
  if [ -f "$HOME/Library/Application Support/Spool/spool.db" ]; then
    ~/spool/bin/spool backup "$HOME/Library/Application Support/Spool/backups/pre-upgrade.db" 2>/dev/null || true
  fi
  chmod 755 ~/spool/bin/spool.new
  mv ~/spool/bin/spool.new ~/spool/bin/spool
  launchctl bootout "gui/$(id -u)/$LABEL" 2>/dev/null || true
  # Wait for the old process to save its state and exit before starting the new one.
  for i in 1 2 3 4 5 6 7 8 9 10 11 12 13 14 15; do
    pgrep -f "spool/bin/spool serve" >/dev/null || break
    sleep 1
  done
  launchctl bootstrap "gui/$(id -u)" ~/Library/LaunchAgents/"$LABEL".plist
  sleep 3
  ~/spool/bin/spool --version
  tail -n 3 ~/Library/Logs/spool.log
REMOTE
echo "Deployed. Open http://$HOST:${BIND##*:}"
