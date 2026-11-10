#!/usr/bin/env bash
# Installs a conduit .deb in clean Debian containers and checks what a user depends on (issue #669).
#
#   scripts/test-deb.sh PACKAGE.deb [IMAGE ...]      default images: debian:12 debian:13
#   EXPECT_VERSION=2.3.0 scripts/test-deb.sh ...     also checks that `conduit --version` says so
#   UPGRADE_FROM=old.deb scripts/test-deb.sh ...     also installs this older conduit-proxy package first and
#                                                    checks that PACKAGE.deb replaces it and keeps the configuration
#
# Needs Docker. For each image: the package installs; the binary runs; the sample configuration validates; the
# account, the directories and the permissions are right; the unit file is accepted by systemd-analyze; the service
# is neither enabled nor started; `remove` keeps an edited configuration and `purge` deletes it; the package installs
# again afterwards; and lintian reports no error.
set -euo pipefail

if [ "$#" -lt 1 ]; then
  echo "usage: $0 PACKAGE.deb [IMAGE ...]" >&2
  exit 2
fi
DEB=$(realpath "$1")
shift
IMAGES=("$@")
if [ "${#IMAGES[@]}" -eq 0 ]; then IMAGES=(debian:12 debian:13); fi

for image in "${IMAGES[@]}"; do
  echo "== $image"
  docker run --rm -e EXPECT_VERSION="${EXPECT_VERSION:-}" -v "$DEB:/pkg.deb:ro" "$image" bash -euo pipefail -c '
    export DEBIAN_FRONTEND=noninteractive
    apt-get update -qq
    apt-get install -y -qq --no-install-recommends /pkg.deb systemd lintian >/dev/null

    conduit --version
    if [ -n "$EXPECT_VERSION" ]; then conduit --version | grep -q "$EXPECT_VERSION"; fi
    conduit validate -c /etc/conduit/conduit.yaml

    getent passwd conduit | grep -q nologin
    [ "$(stat -c "%U:%G %a" /etc/conduit/conduit.yaml)" = "root:conduit 640" ]
    [ "$(stat -c "%U:%G %a" /var/log/conduit)" = "conduit:conduit 750" ]
    [ "$(stat -c "%U:%G %a" /var/cache/conduit)" = "conduit:conduit 750" ]
    grep -q "^ExecStart=/usr/bin/conduit -c /etc/conduit/conduit.yaml" /usr/lib/systemd/system/conduit.service
    systemd-analyze verify /usr/lib/systemd/system/conduit.service

    # Neither enabled nor started: the sample configuration proxies to an application that is not there.
    [ ! -e /etc/systemd/system/multi-user.target.wants/conduit.service ]

    lintian --fail-on error /pkg.deb

    # remove keeps a configuration the user edited; purge deletes it, the log and the cache.
    echo "# edited by the user" >> /etc/conduit/conduit.yaml
    apt-get remove -y -qq conduit >/dev/null
    [ ! -e /usr/bin/conduit ]
    grep -q "# edited by the user" /etc/conduit/conduit.yaml
    apt-get purge -y -qq conduit >/dev/null
    # (one check per line: in an `a && b && c` list `set -e` stops only when the last one fails)
    [ ! -e /etc/conduit ]
    [ ! -e /var/log/conduit ]
    [ ! -e /var/cache/conduit ]
    getent passwd conduit >/dev/null   # the account stays

    # Installing again works (the account already exists).
    apt-get install -y -qq /pkg.deb >/dev/null
    conduit --version >/dev/null

    # A configuration the administrator deleted stays deleted, and the package still configures (postinst must not
    # fail on the missing file).
    rm /etc/conduit/conduit.yaml
    apt-get install -y -qq --reinstall /pkg.deb >/dev/null
    [ ! -e /etc/conduit/conduit.yaml ]
    dpkg -s conduit | grep -q "^Status: install ok installed"
    echo "ok"
  '

  # The package was called conduit-proxy in 2.2.0: installing this one replaces it and keeps the edited configuration.
  if [ -n "${UPGRADE_FROM:-}" ]; then
    echo "== $image: upgrade from $(basename "$UPGRADE_FROM")"
    docker run --rm -v "$DEB:/pkg.deb:ro" -v "$(realpath "$UPGRADE_FROM"):/old.deb:ro" "$image" bash -euo pipefail -c '
      export DEBIAN_FRONTEND=noninteractive
      apt-get update -qq
      apt-get install -y -qq --no-install-recommends /old.deb >/dev/null
      dpkg -s conduit-proxy | grep -q "^Status: install ok installed"
      echo "# edited before the rename" >> /etc/conduit/conduit.yaml

      # The configuration was edited, so dpkg would ask which version to keep (the shipped file differs from 2.2.0 in its
      # first comment line); an interactive user presses N, this run answers keep-mine.
      apt-get install -y -qq -o Dpkg::Options::=--force-confold /pkg.deb >/dev/null
      dpkg -s conduit | grep -q "^Status: install ok installed"
      # (dpkg keeps the removed package as `config-files`, "rc" in dpkg -l: its conffile record, not an installation)
      [ "$(dpkg-query -W -f="\${db:Status-Status}" conduit-proxy 2>/dev/null || echo not-installed)" != "installed" ]
      [ "$(dpkg-query -W -f="\${db:Status-Status}" conduit-proxy)" = "config-files" ]
      conduit --version >/dev/null
      grep -q "# edited before the rename" /etc/conduit/conduit.yaml
      [ "$(stat -c "%U:%G %a" /etc/conduit/conduit.yaml)" = "root:conduit 640" ]
      conduit validate -c /etc/conduit/conduit.yaml >/dev/null
      [ ! -e /etc/systemd/system/multi-user.target.wants/conduit.service ]

      # Purging the new package leaves nothing of either.
      apt-get purge -y -qq conduit >/dev/null
      [ ! -e /etc/conduit ]
      [ ! -e /var/log/conduit ]
      echo "ok"
    '
  fi
done
