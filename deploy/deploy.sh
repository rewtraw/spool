#!/bin/sh
# Build Spool and install it as a LaunchAgent on a Mac, over SSH, then restart it.
#
# Settings come from the environment or from deploy/local.env (not tracked):
#
#   SPOOL_HOST           SSH host to install on (required)
#   SPOOL_SIGN_IDENTITY  codesign identity; unset signs ad hoc
#   SPOOL_LABEL          LaunchAgent label and signing identifier (default: local.spool)
#   SPOOL_BIND           address to listen on (default: 0.0.0.0:7979)
#   SPOOL_API_KEY        Spool's key, if the app has a password, so the script may ask it things
#   SPOOL_FORCE          set to restart even though Spool says it is in the middle of something
#   SPOOL_WAIT_MINUTES   how long to wait for compact versions being made to finish (default: 90)
#   SPOOL_HELPER_HOST    if set, the helper on that machine is updated afterwards; see deploy/helper.sh
#
# A Spool that is already running is asked first whether a restart would interrupt anything:
# a download being unpacked, an import, a title being moved, someone playing. If so the
# script stops and says what. Compact versions being made are waited for. Downloads still coming down are paused, and resumed afterwards.
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

URL="http://$HOST:${BIND##*:}"
ask() { curl -fsS -m 8 ${SPOOL_API_KEY:+-H "X-Api-Key: $SPOOL_API_KEY"} "$@"; }
RESUME=""
# Spool not running at all is the only answer that lets this go ahead unasked. One that
# refuses to say whether it is busy has a password, and the key is needed to ask.
if ! STATE=$(ask "$URL/api/busy" 2>&1); then
  case "$STATE" in
    *"Failed to connect"*|*"Couldn't connect"*|*"Connection refused"*) STATE="" ;;
    *)
      echo "Could not ask Spool on $HOST whether it is busy: $STATE"
      echo "If it has a password, put its key in deploy/local.env as SPOOL_API_KEY."
      exit 1
      ;;
  esac
fi
if [ -n "$STATE" ]; then
  case "$STATE" in
    *'"busy":true'*)
      echo "Spool on $HOST is in the middle of something:"
      printf '%s\n' "$STATE" | sed -e 's/.*"reasons":\[//' -e 's/\].*//' -e 's/","/\n/g' -e 's/"//g' | sed 's/^/  - /'
      if [ -z "${SPOOL_FORCE:-}" ]; then
        echo "Not restarting it. Try again when that is done, or set SPOOL_FORCE=1 to restart anyway."
        exit 1
      fi
      ;;
  esac
  # A compact version cut short is begun again from nothing. No more are begun, those
  # being made are waited for, and the line is taken up again by the Spool that starts.
  making() { ask "$URL/api/busy" 2>/dev/null | sed -n 's/.*"making":\([0-9]*\).*/\1/p'; }
  if [ "$(making)" -gt 0 ] 2>/dev/null && [ -z "${SPOOL_FORCE:-}" ]; then
    ask -X POST -H 'Content-Type: application/json' -d '{"hold":true}' "$URL/api/compact/hold" >/dev/null
    echo "Waiting for $(making) compact version(s) being made to finish; no more are begun until Spool has restarted."
    LEFT=$(( ${SPOOL_WAIT_MINUTES:-90} * 4 ))
    while [ "$(making)" -gt 0 ] 2>/dev/null; do
      LEFT=$((LEFT - 1))
      if [ "$LEFT" -le 0 ]; then
        ask -X POST -H 'Content-Type: application/json' -d '{"hold":false}' "$URL/api/compact/hold" >/dev/null
        echo "They are still being made. Not restarting; the line carries on. Try again later, or set SPOOL_FORCE=1."
        exit 1
      fi
      sleep 15
    done
  fi
  # Downloads resume from where they stopped; pausing first lets them stop at a clean point.
  case "$(ask "$URL/api/status" 2>/dev/null)" in
    *'"queue_paused":false'*) ask -X POST "$URL/api/queue/pause" >/dev/null 2>&1 && RESUME=1 ;;
  esac
fi

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
  # Loading it does not always start it (seen after casper restarted), so it is started too.
  launchctl kickstart "gui/$(id -u)/$LABEL" 2>/dev/null || true
  sleep 3
  ~/spool/bin/spool --version
  tail -n 3 ~/Library/Logs/spool.log
REMOTE
if [ -n "$RESUME" ]; then
  for i in 1 2 3 4 5 6 7 8 9 10; do
    ask -X POST "$URL/api/queue/resume" >/dev/null 2>&1 && break
    sleep 1
  done
fi
echo "Deployed. Open $URL"
# A helper on another machine is kept at the same version.
if [ -n "${SPOOL_HELPER_HOST:-}" ]; then
  deploy/helper.sh || echo "Spool is deployed, but its helper was not updated; run deploy/helper.sh when it can be."
fi
