#!/usr/bin/env bash
# Builds the conduit .deb from a built conduit binary (issue #669).
#
#   scripts/build-deb.sh BINARY ARCH VERSION OUTDIR
#     BINARY   a static conduit binary (the x86_64/aarch64 musl full build)
#     ARCH     the Debian architecture: amd64 or arm64
#     VERSION  the release version, 2.2.0 (a prerelease 2.2.0-rc.1 becomes 2.2.0~rc.1, which sorts before 2.2.0)
#     OUTDIR   where conduit_<version>_<arch>.deb is written
#
# Needs dpkg-deb, gzip and xz. Contents: /usr/bin/conduit; the systemd unit of contrib/conduit.service with the
# binary path of a package; /etc/conduit/conduit.yaml (a conffile: kept on upgrade and remove, deleted on purge);
# maintainer scripts that create the `conduit` account and its directories; the copyright file. The service is not
# enabled and not started (see contrib/deb/postinst).
set -euo pipefail

if [ "$#" -ne 4 ]; then
  echo "usage: $0 BINARY ARCH VERSION OUTDIR" >&2
  exit 2
fi
BINARY=$1 ARCH=$2 VERSION=$3 OUTDIR=$4
case "$ARCH" in amd64|arm64) ;; *) echo "ARCH must be amd64 or arm64, got: $ARCH" >&2; exit 2 ;; esac
[ -x "$BINARY" ] || { echo "$BINARY is not an executable file" >&2; exit 2; }
# The version goes into a sed script and a file name: only a semantic version is accepted (a git tag may contain
# anything), 2.2.0 or 2.2.0-rc.1.
if ! [[ "$VERSION" =~ ^[0-9]+\.[0-9]+\.[0-9]+(-[0-9A-Za-z.]+)?$ ]]; then
  echo "VERSION must be like 2.2.0 or 2.2.0-rc.1, got: $VERSION" >&2
  exit 2
fi
DEB_VERSION=${VERSION//-/\~}

HERE=$(cd "$(dirname "$0")/.." && pwd)
STAGE=$(mktemp -d)
trap 'rm -rf "$STAGE"' EXIT
chmod 0755 "$STAGE"   # the root of the package becomes the mode of / on install; mktemp makes it 0700

install -D -m 0755 "$BINARY" "$STAGE/usr/bin/conduit"
mkdir -p "$STAGE/usr/lib/systemd/system"
sed -e 's#^ExecStart=/usr/local/bin/conduit$#ExecStart=/usr/bin/conduit -c /etc/conduit/conduit.yaml#' -e 's#/usr/local/bin/conduit#/usr/bin/conduit#g' "$HERE/contrib/conduit.service" > "$STAGE/usr/lib/systemd/system/conduit.service"
chmod 0644 "$STAGE/usr/lib/systemd/system/conduit.service"
install -D -m 0640 "$HERE/contrib/deb/conduit.yaml" "$STAGE/etc/conduit/conduit.yaml"
install -D -m 0644 "$HERE/contrib/deb/copyright" "$STAGE/usr/share/doc/conduit/copyright"
install -D -m 0644 "$HERE/contrib/deb/lintian-overrides" "$STAGE/usr/share/lintian/overrides/conduit"
# A native package needs a changelog in Debian's format; the history of the project is the release notes.
{
  printf 'conduit (%s) unstable; urgency=medium\n\n' "$DEB_VERSION"
  printf '  * Release %s.\n' "$VERSION"
  printf '    Notes: https://github.com/lopatnov/conduit/releases/tag/v%s\n\n' "$VERSION"
  printf ' -- Oleksandr Lopatnov <lopatniov@gmail.com>  %s\n' "$(date -u -R ${SOURCE_DATE_EPOCH:+-d "@$SOURCE_DATE_EPOCH"})"
} | gzip -9n > "$STAGE/usr/share/doc/conduit/changelog.gz"
chmod 0644 "$STAGE/usr/share/doc/conduit/changelog.gz"

mkdir -p "$STAGE/DEBIAN"
for script in postinst prerm postrm; do
  install -m 0755 "$HERE/contrib/deb/$script" "$STAGE/DEBIAN/$script"
done
echo /etc/conduit/conduit.yaml > "$STAGE/DEBIAN/conffiles"
SIZE=$(du -sk --exclude=DEBIAN "$STAGE" | cut -f1)
sed -e "s/@VERSION@/$DEB_VERSION/" -e "s/@ARCH@/$ARCH/" -e "s/@SIZE@/$SIZE/" "$HERE/contrib/deb/control.in" > "$STAGE/DEBIAN/control"

mkdir -p "$OUTDIR"
OUT="$OUTDIR/conduit_${DEB_VERSION}_${ARCH}.deb"
dpkg-deb --root-owner-group -Zxz --build "$STAGE" "$OUT" >/dev/null
echo "$OUT"
