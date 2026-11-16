#!/usr/bin/env bash
#
# Starts ScyllaDB taking only TLS clients on port 19043, with the
# certificate for "localhost" that scripts/tls-postgres.sh made (run that
# first), for the Cassandra TLS tests.
#
#   ./scripts/tls-scylla.sh          start it
#   ./scripts/tls-scylla.sh down     stop it

set -euo pipefail

cd "$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

name=dfox-scylla-tls

if [ "${1:-}" = down ]; then
	docker rm -f "$name" >/dev/null 2>&1 || true
	exit 0
fi

dir=target/tls
[ -f "$dir/server.pem" ] || { echo "tls-scylla: run ./scripts/tls-postgres.sh first" >&2; exit 1; }

docker rm -f "$name" >/dev/null 2>&1 || true
docker run -d --name "$name" -p 19043:9042 -v "$PWD/$dir:/certs:ro" \
	--entrypoint bash scylladb/scylla:2025.1 -c '
		printf "\nclient_encryption_options:\n  enabled: true\n  certificate: /certs/server.pem\n  keyfile: /certs/server.key\n" \
			>>/etc/scylla/scylla.yaml &&
		exec /docker-entrypoint.py --smp 1 --memory 750M --overprovisioned 1 --developer-mode 1' >/dev/null

for _ in $(seq 120); do
	if docker logs "$name" 2>&1 | grep -q "Starting listening for CQL clients.*encrypted"; then
		echo "TLS ScyllaDB is up on 19043"
		exit 0
	fi
	sleep 1
done
echo "tls-scylla: the server didn't start" >&2
docker logs "$name" >&2
exit 1
