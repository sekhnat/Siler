#!/usr/bin/env bash
# Copies the third-party code the extension's YouTube recogniser runs, unmodified, into
# extension/media/youtube/vendor/:
#   - yt.solver.core.js from yt-dlp/ejs (Unlicense), taken from a yt-dlp source zip and
#     checked against the SHA3-512 that yt-dlp records for it (vendor/_info.py);
#   - meriyah and astring from npm, at the versions that ejs build pins.
#
# Usage: scripts/vendor-extension-libs.sh path/to/yt-dlp-master.zip
set -euo pipefail

ZIP=${1:?usage: $0 path/to/yt-dlp-master.zip}
MERIYAH=6.1.4
ASTRING=1.9.0

ROOT=$(cd "$(dirname "$0")/.." && pwd)
OUT="$ROOT/extension/media/youtube/vendor"
WORK=$(mktemp -d)
trap 'rm -rf "$WORK"' EXIT

PREFIX=$(unzip -Z1 "$ZIP" | grep -m1 'yt_dlp/extractor/youtube/jsc/_builtin/vendor/yt.solver.core.js$' | sed 's#yt.solver.core.js$##')
[ -n "$PREFIX" ] || { echo "no ejs solver in $ZIP" >&2; exit 1; }
unzip -p "$ZIP" "${PREFIX}yt.solver.core.js" > "$WORK/yt.solver.core.js"
unzip -p "$ZIP" "${PREFIX}_info.py" > "$WORK/_info.py"

python3 -I - "$WORK" <<'EOF'
import hashlib, re, sys
work = sys.argv[1]
info = open(f"{work}/_info.py").read()
want = re.search(r"'yt.solver.core.js':\s*'([0-9a-f]+)'", info).group(1)
version = re.search(r"VERSION\s*=\s*'([^']+)'", info).group(1)
got = hashlib.sha3_512(open(f"{work}/yt.solver.core.js", "rb").read()).hexdigest()
if got != want:
    sys.exit(f"yt.solver.core.js doesn't match yt-dlp's recorded hash")
open(f"{work}/EJS_VERSION", "w").write(version)
print(f"ejs {version}: hash verified")
EOF

( cd "$WORK" && npm pack --silent "meriyah@$MERIYAH" "astring@$ASTRING" >/dev/null )
mkdir -p "$WORK/m" "$WORK/a"
tar -xzf "$WORK/meriyah-$MERIYAH.tgz" -C "$WORK/m"
tar -xzf "$WORK/astring-$ASTRING.tgz" -C "$WORK/a"

mkdir -p "$OUT"
cp "$WORK/yt.solver.core.js" "$OUT/yt.solver.core.js"
cp "$WORK/m/package/dist/meriyah.umd.min.js" "$OUT/meriyah.umd.min.js"
cp "$WORK/m/package/LICENSE.md" "$OUT/LICENSE.meriyah.md"
cp "$WORK/a/package/dist/astring.min.js" "$OUT/astring.min.js"
cp "$WORK/a/package/LICENSE" "$OUT/LICENSE.astring"
EJS=$(cat "$WORK/EJS_VERSION")

cat > "$OUT/SOURCES.md" <<EOF
# Third-party code in this folder

Copied unmodified by \`scripts/vendor-extension-libs.sh\`. Nothing here is built or changed.

| File | Source | Version | License |
|---|---|---|---|
| \`yt.solver.core.js\` | [yt-dlp/ejs](https://github.com/yt-dlp/ejs), as vendored by yt-dlp (\`yt_dlp/extractor/youtube/jsc/_builtin/vendor/\`), SHA3-512 checked against yt-dlp's \`_info.py\` | $EJS | Unlicense (public domain), see the file's header |
| \`meriyah.umd.min.js\` | npm [\`meriyah\`](https://www.npmjs.com/package/meriyah) \`dist/meriyah.umd.min.js\` | $MERIYAH | ISC, \`LICENSE.meriyah.md\` |
| \`astring.min.js\` | npm [\`astring\`](https://www.npmjs.com/package/astring) \`dist/astring.min.js\` | $ASTRING | MIT, \`LICENSE.astring\` |

SHA-256:

\`\`\`
$(cd "$OUT" && sha256sum yt.solver.core.js meriyah.umd.min.js astring.min.js)
\`\`\`
EOF
echo "vendored into $OUT"
