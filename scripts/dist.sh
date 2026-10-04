#!/usr/bin/env bash
# NewGit release packaging — zero-rupee, no external services.
#
#   scripts/dist.sh [--skip-build]
#
# Produces dist/newgit-<version>-<target-triple>/ with the release binary,
# README, LICENSE(S), docs/, SBOM.md, plus a sha256 checksums file and a
# tarball. Deterministic inputs: Cargo.lock is committed; the release profile
# is strip+lto=thin (docs/BENCHMARKS.md records same-host bit-reproducibility).
set -euo pipefail

cd "$(dirname "$0")/.."
ROOT="$PWD"

SKIP_BUILD=0
[[ "${1:-}" == "--skip-build" ]] && SKIP_BUILD=1

VERSION="$(grep -m1 '^version' Cargo.toml | sed 's/.*"\(.*\)".*/\1/')"
TARGET="$(rustc -vV | sed -n 's|host: ||p')"
NAME="newgit-${VERSION}-${TARGET}"
OUT="dist/${NAME}"

if [[ $SKIP_BUILD -eq 0 ]]; then
    cargo build --release --bin newgit --bin newgit-faultlab --bin newgit-bench
fi

rm -rf "$OUT"
mkdir -p "$OUT/bin" "$OUT/docs"
cp target/release/newgit            "$OUT/bin/"
cp target/release/newgit-faultlab   "$OUT/bin/"   # crash-test harness (used by the test suite)
cp target/release/newgit-bench      "$OUT/bin/"   # benchmark harness
cp README.md CHANGELOG.md SBOM.md   "$OUT/"
cp -r docs/.                        "$OUT/docs/"
# state/audit documents ship too — they are part of the product's honesty story
cp PROJECT_STATE.md ROADMAP.md DECISIONS.md ARCHITECTURE.md SECURITY_MODEL.md \
   THREAT_MODEL.md TEST_MATRIX.md RELEASE_READINESS.md KNOWN_LIMITATIONS.md \
   "$OUT/" 2>/dev/null || true
for f in LICENSE-MIT LICENSE-APACHE; do
    if [[ ! -f "$f" ]]; then echo "FATAL: $f missing (Cargo.toml declares MIT OR Apache-2.0)" >&2; exit 1; fi
    cp "$f" "$OUT/"
done

( cd "$OUT" && find . -type f ! -name SHA256SUMS.txt -print0 | sort -z | xargs -0 sha256sum > SHA256SUMS.txt )
( cd dist && tar -czf "${NAME}.tar.gz" "$NAME" && sha256sum "${NAME}.tar.gz" > "${NAME}.tar.gz.sha256" )

echo "dist ready:"
echo "  ${OUT}/ (SHA256SUMS.txt inside)"
echo "  dist/${NAME}.tar.gz (+ .sha256)"
sha256sum "dist/${NAME}.tar.gz" | sed 's/^/  /'
"$OUT/bin/newgit" --version
