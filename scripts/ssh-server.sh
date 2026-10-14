#!/usr/bin/env bash
#
# Starts an SSH jump host on 127.0.0.1:52222 for the tunnel tests. It
# forwards to the host's ports through host.docker.internal, so the test
# Postgres on 55432 is reachable through it. Keys and known_hosts are written
# to target/ssh:
#
#   id_ed25519        authorized, no passphrase
#   id_passphrase     authorized, passphrase "dfox"
#   id_stranger       not authorized
#   known_hosts       the jump host's key
#   other_known_hosts a different key for the jump host, as if it changed
#
# The user is "dfox", with password "dfox".
#
#   ./scripts/ssh-server.sh          start it
#   ./scripts/ssh-server.sh down     stop it

set -euo pipefail

cd "$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

name=dfox-ssh
port=52222

if [ "${1:-}" = down ]; then
	docker rm -f "$name" >/dev/null 2>&1 || true
	exit 0
fi

dir=target/ssh
mkdir -p "$dir"
(
	cd "$dir"
	for key in id_ed25519 id_passphrase id_stranger host_key other_host_key; do
		rm -f "$key" "$key.pub"
	done
	ssh-keygen -q -t ed25519 -N '' -C dfox-test -f id_ed25519
	ssh-keygen -q -t ed25519 -N dfox -C dfox-test -f id_passphrase
	ssh-keygen -q -t ed25519 -N '' -C dfox-test -f id_stranger
	ssh-keygen -q -t ed25519 -N '' -C dfox-test -f host_key
	ssh-keygen -q -t ed25519 -N '' -C dfox-test -f other_host_key
	cat id_ed25519.pub id_passphrase.pub >authorized_keys
	printf '[127.0.0.1]:%s %s\n' "$port" "$(cut -d' ' -f1-2 host_key.pub)" >known_hosts
	printf '[127.0.0.1]:%s %s\n' "$port" "$(cut -d' ' -f1-2 other_host_key.pub)" >other_known_hosts
	chmod 600 id_ed25519 id_passphrase id_stranger host_key other_host_key
)

docker rm -f "$name" >/dev/null 2>&1 || true
# sshd wants its files owned by root and private, so copy them in. The tests
# fail logins on purpose, so sshd must not shut them out for it.
docker run -d --name "$name" -p "127.0.0.1:$port:22" \
	--add-host=host.docker.internal:host-gateway \
	-v "$PWD/$dir:/keys:ro" alpine:3.22 sh -c '
		apk add --no-cache openssh-server >/dev/null &&
		adduser -D -s /bin/sh dfox && echo dfox:dfox | chpasswd &&
		mkdir -p /home/dfox/.ssh &&
		cp /keys/authorized_keys /home/dfox/.ssh/ &&
		chown -R dfox /home/dfox/.ssh && chmod 700 /home/dfox/.ssh &&
		cp /keys/host_key /etc/ssh/ssh_host_ed25519_key && chmod 600 /etc/ssh/ssh_host_ed25519_key &&
		exec /usr/sbin/sshd -D -e \
			-o HostKey=/etc/ssh/ssh_host_ed25519_key \
			-o PasswordAuthentication=yes \
			-o AllowTcpForwarding=yes \
			-o PerSourcePenalties=no' >/dev/null

for _ in $(seq 60); do
	if ssh-keyscan -p "$port" 127.0.0.1 2>/dev/null | grep -q ssh-ed25519; then
		echo "SSH jump host is up on 127.0.0.1:$port; keys in $dir"
		exit 0
	fi
	sleep 1
done
echo "ssh-server: sshd didn't start" >&2
docker logs "$name" >&2
exit 1
