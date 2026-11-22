#!/usr/bin/env bash
# Signs the Firefox extension on addons.mozilla.org (AMO) as "unlisted": Mozilla signs it
# but doesn't publish it. Release builds of Firefox only keep signed extensions installed.
#
# Needs an AMO API key (https://addons.mozilla.org/developers/addon/api/key/), given as
# AMO_JWT_ISSUER and AMO_JWT_SECRET in the environment or in ~/.config/siler/amo.env.
# The extension is uploaded to Mozilla for automated review; every signing needs a new
# "version" in extension/manifest.json.
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
env_file="${XDG_CONFIG_HOME:-$HOME/.config}/siler/amo.env"

if [[ -z "${AMO_JWT_ISSUER:-}" || -z "${AMO_JWT_SECRET:-}" ]] && [[ -f "$env_file" ]]; then
  if [[ "$(stat -c %a "$env_file")" != "600" ]]; then
    echo "warning: $env_file holds a secret; restrict it with: chmod 600 $env_file" >&2
  fi
  set -a
  # shellcheck source=/dev/null
  . "$env_file"
  set +a
fi

if [[ -z "${AMO_JWT_ISSUER:-}" || -z "${AMO_JWT_SECRET:-}" ]]; then
  cat >&2 <<EOF
Missing AMO API credentials.

1. Sign in at https://addons.mozilla.org and create an API key:
   https://addons.mozilla.org/developers/addon/api/key/
2. Save it in $env_file:
     AMO_JWT_ISSUER=user:12345:67
     AMO_JWT_SECRET=0123456789abcdef…
   then: chmod 600 $env_file
3. Run this script again.
EOF
  exit 1
fi

version="$(python3 -c 'import json, sys; print(json.load(open(sys.argv[1]))["version"])' "$root/extension/manifest.json")"
artifacts="$root/extension/web-ext-artifacts"
echo "==> Building the Firefox extension"
node "$root/scripts/build-extension.mjs" firefox >/dev/null
echo "==> Signing Siler Integration $version (unlisted)"
# Credentials go through the environment, not the command line, so they don't show up in ps.
WEB_EXT_API_KEY="$AMO_JWT_ISSUER" WEB_EXT_API_SECRET="$AMO_JWT_SECRET" \
  npx --yes web-ext@10 sign --channel=unlisted --source-dir "$root/dist/extension-firefox" --artifacts-dir "$artifacts"

xpi="$(ls -t "$artifacts"/*.xpi | head -n 1)"
dest="${XDG_DATA_HOME:-$HOME/.local/share}/siler"
mkdir -p "$dest"
cp "$xpi" "$dest/siler-integration.xpi"

echo
echo "Signed: $dest/siler-integration.xpi"
echo "Install it from Siler → Settings → Browser → Add to Firefox, or open the file with Firefox."
