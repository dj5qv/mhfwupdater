#!/bin/sh
# Build binary Debian packages with a snapshot version, e.g.
# 1.1.0~beta.1+20261214.144712.gff27a37. The version comes from Cargo.toml,
# where a pre-release 1.1.0-beta.1 becomes 1.1.0~beta.1, which sorts before
# the final 1.1.0. The timestamp is the UTC commit time, so builds of the same
# commit get the same version on every platform. If tracked files have
# uncommitted changes, the build time is used instead and ".dirty" is
# appended, keeping such builds unique. debian/rules picks the version up from
# MHFWUPDATER_DEB_VERSION.
#
# Release builds: if HEAD carries the tag v<version> (version from
# Cargo.toml, e.g. v1.0.0) and the tree is clean, the packages get the plain
# version from debian/changelog, which must match Cargo.toml.
#
# Arguments are passed to dpkg-buildpackage, e.g. -d to skip the build
# dependency check when cargo comes from a rustup installed outside dpkg.
# The .deb files land in the parent directory of the source tree, as with
# plain dpkg-buildpackage.

set -e
cd "$(dirname "$0")"

release=$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -n 1)
if [ -z "$release" ]; then
	echo "Can't find version in Cargo.toml" >&2
	exit 1
fi
upstream=$(echo "$release" | tr - '~')

if git rev-parse -q --verify "refs/tags/v$release" >/dev/null 2>&1 &&
	[ "$(git rev-parse HEAD)" = "$(git rev-parse "v$release^{commit}")" ] &&
	git diff --quiet HEAD --; then
	changelog=$(dpkg-parsechangelog -S Version)
	if [ "$changelog" != "$upstream" ]; then
		echo "Tag v$release, but debian/changelog has version $changelog" >&2
		exit 1
	fi
	echo "Building Debian packages, release $upstream"
	dpkg-buildpackage -us -uc -b -tc "$@"
	ls -1 ../*_"$upstream"_*.deb
	exit 0
fi

stamp=$(date -u +%Y%m%d.%H%M%S)
suffix=
if rev=$(git rev-parse --short HEAD 2>/dev/null); then
	suffix=.g$rev
	if git diff --quiet HEAD --; then
		stamp=$(TZ=UTC0 git log -1 --date=format-local:%Y%m%d.%H%M%S --format=%cd)
	else
		suffix=$suffix.dirty
	fi
fi
version=$upstream+$stamp$suffix

echo "Building Debian packages, version $version"
MHFWUPDATER_DEB_VERSION=$version dpkg-buildpackage -us -uc -b -tc "$@"
ls -1 ../*_"$version"_*.deb
