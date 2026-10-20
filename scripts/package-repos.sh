#!/usr/bin/env bash
# Builds the APT and dnf repositories served from GitHub Pages, from the .deb
# and .rpm files in $1, into $2. Signs them with the GPG key $KEY (default:
# "dfox packages") from the keyring. Only the given packages are in them:
# both package managers need nothing but the latest version.
#
#   scripts/package-repos.sh out/ site/
#
# Needs apt-ftparchive (apt-utils), createrepo_c, rpmsign (rpm) and gpg.
set -euo pipefail

packages="$(cd "$1" && pwd)"
site="$2"
key="${KEY:-dfox packages}"
base_url="${BASE_URL:-https://0xataru.github.io/dfox}"

rm -rf "$site"
mkdir -p "$site/apt/pool/main" "$site/rpm"
cp "$packages"/*.deb "$site/apt/pool/main/"
cp "$packages"/*.rpm "$site/rpm/"
gpg --batch --armor --export "$key" >"$site/key.asc"
gpg --batch --export "$key" >"$site/key.gpg"
# The site's root sends people to the install instructions.
cat >"$site/index.html" <<'EOF'
<!doctype html>
<meta charset="utf-8">
<title>dfox packages</title>
<meta http-equiv="refresh" content="0; url=https://github.com/0xataru/dfox#installation">
<p>APT and dnf repositories for dfox. See <a href="https://github.com/0xataru/dfox#installation">how to install</a>.</p>
EOF

(
	cd "$site/apt"
	for arch in amd64 arm64; do
		dir="dists/stable/main/binary-$arch"
		mkdir -p "$dir"
		apt-ftparchive --arch "$arch" packages pool >"$dir/Packages"
		gzip -kf "$dir/Packages"
	done
	apt-ftparchive \
		-o APT::FTPArchive::Release::Origin=dfox \
		-o APT::FTPArchive::Release::Label=dfox \
		-o APT::FTPArchive::Release::Suite=stable \
		-o APT::FTPArchive::Release::Codename=stable \
		-o "APT::FTPArchive::Release::Architectures=amd64 arm64" \
		-o APT::FTPArchive::Release::Components=main \
		release dists/stable >Release
	mv Release dists/stable/Release
	gpg --batch --yes -u "$key" --clearsign -o dists/stable/InRelease dists/stable/Release
	gpg --batch --yes -u "$key" --armor --detach-sign -o dists/stable/Release.gpg dists/stable/Release
)

(
	cd "$site/rpm"
	rpmsign --define "_gpg_name $key" --define "__gpg $(command -v gpg)" --addsign ./*.rpm >/dev/null
	createrepo_c --quiet .
	gpg --batch --yes -u "$key" --armor --detach-sign repodata/repomd.xml
	cat >dfox.repo <<EOF
[dfox]
name=dfox
baseurl=$base_url/rpm
enabled=1
gpgcheck=1
repo_gpgcheck=0
gpgkey=$base_url/key.asc
EOF
)
