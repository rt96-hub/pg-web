#!/usr/bin/env bash
#
# Installed at /docker-entrypoint-initdb.d/10-pgweb.sh in the pgweb/postgres
# image. Runs exactly once — during the initial database bootstrap — against
# a temporary postmaster that's serving on a Unix socket.
#
# What we do:
#   1. Append shared_preload_libraries so the NEXT postmaster boot loads the
#      pg_web_ext library and registers its static background worker.
#   2. CREATE EXTENSION in the user's database, which seeds the default
#      route + template. pg-web push replaces these at deploy time.
#
# Why this works: after docker-entrypoint-initdb.d scripts finish, the
# base image's entrypoint stops the temp postmaster and re-execs `postgres`,
# which reads the updated postgresql.conf.

set -e

echo "" >> "$PGDATA/postgresql.conf"
echo "# pg-web: registered by /docker-entrypoint-initdb.d/10-pgweb.sh" >> "$PGDATA/postgresql.conf"
echo "shared_preload_libraries = 'pg_web_ext'" >> "$PGDATA/postgresql.conf"

psql -v ON_ERROR_STOP=1 --username "$POSTGRES_USER" --dbname "$POSTGRES_DB" <<-'EOSQL'
    CREATE EXTENSION IF NOT EXISTS pg_web_ext;
EOSQL

echo "pg-web: extension created in database '$POSTGRES_DB'"
