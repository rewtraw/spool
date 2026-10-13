#!/bin/sh
# Set a Mac up to make compact versions for the Spool on another machine, or update one
# that is. It runs Spool as a helper with no folders, allowed only to make: it is sent a
# film to read, makes the compact version on its video hardware and hands it back.
#
#   deploy/maker.sh HOST      an SSH host, or "local" for this machine
#
# Settings come from the environment or from deploy/local.env (not tracked):
#
#   SPOOL_SIGN_IDENTITY  codesign identity; unset signs ad hoc
#   SPOOL_LABEL          signing identifier, and the start of the LaunchAgent's label (default: local.spool)
#   SPOOL_MAKER_BIND     address to listen on (default: 0.0.0.0:7978)
#
# The first time, it prints a code. In Spool, under Settings, Library, the machine is
# listed as found on the network: type the code there and the two are linked. After that
# it is given films whenever its settings there say it may be.
#
# One that is making a compact version when it is updated loses it; the film goes back in
# Spool's line and is made again.
set -eu
cd "$(dirname "$0")/.."
[ -f deploy/local.env ] && . deploy/local.env
HOST="${1:?say which machine: an SSH host, or local}"
LABEL="${SPOOL_LABEL:-local.spool}"
BIND="${SPOOL_MAKER_BIND:-0.0.0.0:7978}"

on() { if [ "$HOST" = local ]; then sh -c "$1"; else ssh "$HOST" "$1"; fi; }
put() { if [ "$HOST" = local ]; then cp "$1" "$HOME/$2"; else scp -q "$1" "$HOST:$2"; fi; }

on '[ -x /opt/homebrew/bin/ffmpeg ] || command -v ffmpeg >/dev/null' || { echo "ffmpeg is not installed on $HOST; install it first (brew install ffmpeg)."; exit 1; }

(cd web && bun install --frozen-lockfile >/dev/null && bun run build >/dev/null)
cargo build --release -p spool
codesign --force -s "${SPOOL_SIGN_IDENTITY:--}" --identifier "$LABEL" target/release/spool
codesign --verify --strict target/release/spool

REMOTE_HOME=$(on 'printf %s "$HOME"')
PLIST=$(mktemp)
trap 'rm -f "$PLIST"' EXIT
sed -e "s|@LABEL@|$LABEL.maker|g" -e "s|@HOME@|$REMOTE_HOME|g" -e "s|@BIND@|$BIND|g" deploy/maker.plist.template > "$PLIST"

on 'mkdir -p ~/spool/bin ~/Library/LaunchAgents'
put target/release/spool spool/bin/spool.new
put "$PLIST" "Library/LaunchAgents/$LABEL.maker.plist"
on "
  set -e
  chmod 755 ~/spool/bin/spool.new
  mv ~/spool/bin/spool.new ~/spool/bin/spool
  launchctl bootout gui/\$(id -u)/$LABEL.maker 2>/dev/null || true
  for i in 1 2 3 4 5 6 7 8 9 10; do
    pgrep -f 'spool/bin/spool .*helper' >/dev/null || break
    sleep 1
  done
  launchctl bootstrap gui/\$(id -u) ~/Library/LaunchAgents/$LABEL.maker.plist
  sleep 3
  ~/spool/bin/spool --version
  if [ -f \"\$HOME/Library/Application Support/Spool Maker/link.json\" ]; then
    echo 'It is linked already, and carries on.'
  else
    grep 'not linked yet' ~/Library/Logs/spool-maker.log | tail -n 1 | perl -pe 's/\e\[[0-9;]*m//g; s/.*code=/Its code: /'
  fi
"
echo "$HOST is set up to make compact versions."
