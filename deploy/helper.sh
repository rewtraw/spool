#!/bin/sh
# Update Spool's helper on a TrueNAS SCALE machine, over SSH: pull the image that matches
# this checkout and restart the helper on it.
#
# Settings come from the environment or from deploy/local.env (not tracked):
#
#   SPOOL_HELPER_HOST   SSH host the helper runs on, as root or an admin (required)
#   SPOOL_HELPER_URL    where the helper answers, such as http://nas:7978 (required)
#   SPOOL_HELPER_KEY    the helper's key (required)
#   SPOOL_HELPER_APP    the app's name on TrueNAS (default: spool-helper)
#   SPOOL_FORCE         set to restart even though the helper says it is in the middle of something
#
# The image is built by GitHub when main is pushed, which takes a few minutes. If the `gh`
# tool is here, the script waits for that build before pulling. The helper is asked first
# whether a restart would interrupt anything: a download being unpacked, a file being filed,
# a title being moved. Downloads still coming down are checkpointed and carry on.
set -eu
cd "$(dirname "$0")/.."
[ -f deploy/local.env ] && . deploy/local.env
HOST="${SPOOL_HELPER_HOST:?set SPOOL_HELPER_HOST, or put it in deploy/local.env}"
URL="${SPOOL_HELPER_URL:?set SPOOL_HELPER_URL}"
KEY="${SPOOL_HELPER_KEY:?set SPOOL_HELPER_KEY}"
APP="${SPOOL_HELPER_APP:-spool-helper}"
ask() { curl -fsS -m 8 -H "Authorization: Bearer $KEY" "$@"; }

if command -v gh >/dev/null 2>&1; then
  SHA=$(git rev-parse HEAD)
  RUN=$(gh run list --workflow image.yml --commit "$SHA" --limit 1 --json databaseId,status --jq '.[0] | "\(.databaseId) \(.status)"' 2>/dev/null || true)
  case "$RUN" in
    "") echo "No image build was found for this commit; pulling whatever is latest." ;;
    *completed) ;;
    *) echo "Waiting for the image of this commit to be built..."; gh run watch "${RUN%% *}" --exit-status >/dev/null ;;
  esac
fi

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

ssh "$HOST" "midclt call --job app.pull_images '$APP' '{\"redeploy\": true}'" >/dev/null
for i in $(seq 1 30); do
  if INFO=$(ask "$URL/link/work/v1/info" 2>/dev/null); then
    echo "The helper on $HOST is updated: $(printf '%s' "$INFO" | sed -e 's/.*"version":"\([^"]*\)".*/version \1/')"
    exit 0
  fi
  sleep 2
done
echo "The helper on $HOST did not answer after the update."
exit 1
