#!/usr/bin/env bash
#
# Builds the demo world the tapes record: a SQLite shop database, a saved
# connection to it, and empty history, all under target/demo/.
#
#   ./examples/tapes/setup.sh

set -euo pipefail

cd "$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"

demo=target/demo
rm -rf "$demo"
mkdir -p "$demo/config" "$demo/state"

sqlite3 "$demo/shop.db" <<'SQL'
CREATE TABLE customers (
    id INTEGER PRIMARY KEY,
    name TEXT NOT NULL,
    email TEXT NOT NULL,
    country TEXT,
    joined DATE
);
CREATE TABLE products (
    id INTEGER PRIMARY KEY,
    name TEXT NOT NULL,
    price NUMERIC NOT NULL,
    tags TEXT
);
CREATE TABLE orders (
    id INTEGER PRIMARY KEY,
    customer_id INTEGER REFERENCES customers (id),
    product_id INTEGER REFERENCES products (id),
    quantity INTEGER NOT NULL,
    paid BOOLEAN NOT NULL,
    created TIMESTAMP NOT NULL
);
CREATE VIEW big_spenders AS
    SELECT c.name, sum(o.quantity * p.price) AS spent
    FROM orders o JOIN customers c ON c.id = o.customer_id JOIN products p ON p.id = o.product_id
    GROUP BY c.name ORDER BY spent DESC LIMIT 10;

WITH RECURSIVE n(i) AS (SELECT 1 UNION ALL SELECT i + 1 FROM n WHERE i < 1200)
INSERT INTO customers (name, email, country, joined)
SELECT 'customer ' || i, 'c' || i || '@example.com',
       CASE i % 4 WHEN 0 THEN 'UA' WHEN 1 THEN 'DE' WHEN 2 THEN 'US' ELSE NULL END,
       date('2024-01-01', '+' || (i % 600) || ' days')
FROM n;

WITH RECURSIVE n(i) AS (SELECT 1 UNION ALL SELECT i + 1 FROM n WHERE i < 40)
INSERT INTO products (name, price, tags)
SELECT 'product ' || i, round(5 + (i * 7.31) % 200, 2),
       json_array('tag' || (i % 5), 'tag' || (i % 3))
FROM n;

WITH RECURSIVE n(i) AS (SELECT 1 UNION ALL SELECT i + 1 FROM n WHERE i < 50000)
INSERT INTO orders (customer_id, product_id, quantity, paid, created)
SELECT 1 + (i * 37) % 1200, 1 + (i * 11) % 40, 1 + i % 5, i % 7 <> 0,
       datetime('2024-01-01', '+' || ((i * 13) % 50000) || ' minutes')
FROM n;
SQL

cat > "$demo/config/connections.toml" <<TOML
[[connection]]
name = "shop"
driver = "sqlite"
database = "$PWD/$demo/shop.db"
tag = "green"
TOML

echo "demo ready in $demo"
