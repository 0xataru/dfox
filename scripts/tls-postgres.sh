#!/usr/bin/env bash
#
# Starts PostgreSQL with TLS on port 15433, with a certificate for
# "localhost" signed by a throwaway test CA, for the verify-full tests. The
# CA is written to target/tls/ca.pem.
#
#   ./scripts/tls-postgres.sh          start it
#   ./scripts/tls-postgres.sh down     stop it

set -euo pipefail

cd "$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

name=dfox-postgres-tls

if [ "${1:-}" = down ]; then
	docker rm -f "$name" >/dev/null 2>&1 || true
	exit 0
fi

dir=target/tls
mkdir -p "$dir"
(
	cd "$dir"
	openssl req -x509 -newkey rsa:2048 -nodes -days 30 -subj "/CN=dfox test CA" \
		-keyout ca.key -out ca.pem 2>/dev/null
	openssl req -newkey rsa:2048 -nodes -subj "/CN=localhost" \
		-keyout server.key -out server.csr 2>/dev/null
	# Only the name "localhost": connecting by IP must fail verification.
	printf 'subjectAltName=DNS:localhost\n' >server.ext
	openssl x509 -req -in server.csr -CA ca.pem -CAkey ca.key -CAcreateserial \
		-days 30 -extfile server.ext -out server.pem 2>/dev/null
)

docker rm -f "$name" >/dev/null 2>&1 || true
# The server wants its key owned by postgres and private, so copy it in.
docker run -d --name "$name" -p 15433:5432 -e POSTGRES_PASSWORD=dfox \
	-v "$PWD/$dir:/certs:ro" --entrypoint sh postgres:16 -c '
		cp /certs/server.pem /certs/server.key /var/lib/postgresql/ &&
		chown postgres /var/lib/postgresql/server.pem /var/lib/postgresql/server.key &&
		chmod 600 /var/lib/postgresql/server.key &&
		exec docker-entrypoint.sh postgres -c ssl=on \
			-c ssl_cert_file=/var/lib/postgresql/server.pem \
			-c ssl_key_file=/var/lib/postgresql/server.key' >/dev/null

for _ in $(seq 60); do
	if docker exec "$name" psql -U postgres -h 127.0.0.1 -c 'SELECT 1' >/dev/null 2>&1; then
		echo "TLS Postgres is up on 15433; CA in $dir/ca.pem"
		exit 0
	fi
	sleep 1
done
echo "tls-postgres: the server didn't start" >&2
docker logs "$name" >&2
exit 1
