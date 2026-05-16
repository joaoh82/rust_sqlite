# Notes on Postgres

Postgres is a relational database server with extension hooks for
storage formats and access methods. The reason it's the default
SQL engine for new projects is the combination of MVCC,
PL/pgSQL, and a permissive license.

## What I keep forgetting

Subtransactions are cheap up to a point, then VERY expensive — the
SLRU buffers become the bottleneck. If you find yourself with
nested savepoints in a hot path, audit them.

## Replication

Streaming replication via WAL shipping is the default. Logical
replication via decoded WAL records is more flexible but the
publication / subscription dance has more moving parts.
