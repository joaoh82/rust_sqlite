"""End-to-end tests for the sqlrite Python bindings.

Run after `maturin develop` (or against an installed wheel) via:

    python -m pytest sdk/python/tests/

These walk the full PyO3 → Rust → SQLRite pipeline, so a passing
suite is strong evidence the Python SDK is usable.
"""

from __future__ import annotations

import os
import tempfile

import pytest

import sqlrite


# ---------------------------------------------------------------------------
# Fixtures


@pytest.fixture
def conn():
    c = sqlrite.connect(":memory:")
    yield c
    c.close()


@pytest.fixture
def tmp_db(tmp_path):
    """A fresh file path for a file-backed DB. The path is cleaned
    up automatically when the test finishes."""
    path = str(tmp_path / "test.sqlrite")
    yield path
    # tmp_path is auto-cleaned, but be explicit about the sidecar too.
    for p in (path, path + "-wal"):
        if os.path.exists(p):
            try:
                os.remove(p)
            except OSError:
                pass


# ---------------------------------------------------------------------------
# Basic CRUD + iteration


def test_version_exposed():
    assert sqlrite.__version__


def test_in_memory_roundtrip(conn):
    cur = conn.cursor()
    cur.execute(
        "CREATE TABLE users (id INTEGER PRIMARY KEY, name TEXT, age INTEGER)"
    )
    cur.execute("INSERT INTO users (name, age) VALUES ('alice', 30)")
    cur.execute("INSERT INTO users (name, age) VALUES ('bob', 25)")
    cur.execute("SELECT id, name, age FROM users")
    rows = cur.fetchall()
    assert len(rows) == 2
    assert rows[0] == (1, "alice", 30)
    assert rows[1] == (2, "bob", 25)


def test_iteration_produces_tuples(conn):
    cur = conn.cursor()
    cur.execute("CREATE TABLE t (x INTEGER PRIMARY KEY)")
    cur.execute("INSERT INTO t (x) VALUES (1)")
    cur.execute("INSERT INTO t (x) VALUES (2)")
    cur.execute("INSERT INTO t (x) VALUES (3)")
    cur.execute("SELECT x FROM t")
    collected = [r[0] for r in cur]
    assert collected == [1, 2, 3]


def test_description_lists_column_names(conn):
    cur = conn.cursor()
    cur.execute("CREATE TABLE t (a INTEGER PRIMARY KEY, b TEXT)")
    cur.execute("INSERT INTO t (a, b) VALUES (1, 'x')")
    cur.execute("SELECT a, b FROM t")
    assert cur.description is not None
    names = [col[0] for col in cur.description]
    assert names == ["a", "b"]


def test_fetchone_returns_none_when_exhausted(conn):
    cur = conn.cursor()
    cur.execute("CREATE TABLE t (x INTEGER PRIMARY KEY)")
    cur.execute("INSERT INTO t (x) VALUES (42)")
    cur.execute("SELECT x FROM t")
    assert cur.fetchone() == (42,)
    assert cur.fetchone() is None


def test_fetchmany_respects_size(conn):
    cur = conn.cursor()
    cur.execute("CREATE TABLE t (x INTEGER PRIMARY KEY)")
    for i in range(1, 6):
        cur.execute(f"INSERT INTO t (x) VALUES ({i})")
    cur.execute("SELECT x FROM t")
    batch = cur.fetchmany(3)
    assert [r[0] for r in batch] == [1, 2, 3]
    rest = cur.fetchall()
    assert [r[0] for r in rest] == [4, 5]


# ---------------------------------------------------------------------------
# Transactions + context manager


def test_commit_persists_and_rollback_undoes(conn):
    cur = conn.cursor()
    cur.execute("CREATE TABLE t (x INTEGER PRIMARY KEY)")
    cur.execute("INSERT INTO t (x) VALUES (1)")

    cur.execute("BEGIN")
    assert conn.in_transaction is True
    cur.execute("INSERT INTO t (x) VALUES (2)")
    conn.rollback()
    assert conn.in_transaction is False

    cur.execute("SELECT x FROM t")
    assert [r[0] for r in cur.fetchall()] == [1]


def test_context_manager_commits_on_clean_exit(tmp_db):
    with sqlrite.connect(tmp_db) as conn:
        cur = conn.cursor()
        cur.execute("CREATE TABLE t (x INTEGER PRIMARY KEY)")
        cur.execute("INSERT INTO t (x) VALUES (7)")

    # Reopen from a fresh connection — the row must still be there.
    with sqlrite.connect(tmp_db) as conn2:
        cur = conn2.cursor()
        cur.execute("SELECT x FROM t")
        assert [r[0] for r in cur.fetchall()] == [7]


def test_context_manager_rolls_back_on_exception(tmp_db):
    # Seed.
    with sqlrite.connect(tmp_db) as conn:
        cur = conn.cursor()
        cur.execute("CREATE TABLE t (x INTEGER PRIMARY KEY)")
        cur.execute("INSERT INTO t (x) VALUES (1)")

    # Start a txn, raise, expect the row NOT to land.
    with pytest.raises(RuntimeError):
        with sqlrite.connect(tmp_db) as conn:
            cur = conn.cursor()
            cur.execute("BEGIN")
            cur.execute("INSERT INTO t (x) VALUES (2)")
            raise RuntimeError("boom")

    with sqlrite.connect(tmp_db) as conn2:
        cur = conn2.cursor()
        cur.execute("SELECT x FROM t")
        assert [r[0] for r in cur.fetchall()] == [1]


# ---------------------------------------------------------------------------
# File-backed + read-only


def test_file_backed_persists_across_connections(tmp_db):
    conn = sqlrite.connect(tmp_db)
    cur = conn.cursor()
    cur.execute("CREATE TABLE items (id INTEGER PRIMARY KEY, label TEXT)")
    cur.execute("INSERT INTO items (label) VALUES ('a')")
    cur.execute("INSERT INTO items (label) VALUES ('b')")
    conn.close()

    conn = sqlrite.connect(tmp_db)
    cur = conn.cursor()
    cur.execute("SELECT label FROM items")
    labels = sorted(r[0] for r in cur.fetchall())
    assert labels == ["a", "b"]
    conn.close()


def test_read_only_connection_rejects_writes(tmp_db):
    with sqlrite.connect(tmp_db) as conn:
        cur = conn.cursor()
        cur.execute("CREATE TABLE t (id INTEGER PRIMARY KEY)")
        cur.execute("INSERT INTO t (id) VALUES (1)")

    ro = sqlrite.connect_read_only(tmp_db)
    assert ro.read_only is True
    with pytest.raises(sqlrite.SQLRiteError) as exc:
        ro.cursor().execute("INSERT INTO t (id) VALUES (2)")
    assert "read-only" in str(exc.value)
    ro.close()


# ---------------------------------------------------------------------------
# Error paths


def test_bad_sql_raises_sqlrite_error(conn):
    with pytest.raises(sqlrite.SQLRiteError):
        conn.cursor().execute("THIS IS NOT SQL")


def test_parameter_binding_raises_type_error(conn):
    cur = conn.cursor()
    cur.execute("CREATE TABLE t (x INTEGER PRIMARY KEY)")
    with pytest.raises(TypeError) as exc:
        cur.execute("INSERT INTO t (x) VALUES (?)", (1,))
    assert "parameter binding" in str(exc.value).lower()


def test_closed_connection_rejects_operations(conn):
    conn.close()
    with pytest.raises(sqlrite.SQLRiteError) as exc:
        conn.cursor().execute("SELECT 1")
    assert "closed" in str(exc.value).lower()


# ---------------------------------------------------------------------------
# Shortcuts / convenience


def test_connection_execute_shortcut(conn):
    conn.execute("CREATE TABLE t (x INTEGER PRIMARY KEY)")
    conn.execute("INSERT INTO t (x) VALUES (99)")
    cur = conn.execute("SELECT x FROM t")
    assert cur.fetchone() == (99,)


def test_executescript_runs_batched_statements(conn):
    cur = conn.cursor()
    cur.executescript(
        """
        CREATE TABLE a (x INTEGER PRIMARY KEY);
        CREATE TABLE b (x INTEGER PRIMARY KEY);
        INSERT INTO a (x) VALUES (1);
        INSERT INTO b (x) VALUES (2);
        """
    )
    cur.execute("SELECT x FROM a")
    assert cur.fetchone() == (1,)
    cur.execute("SELECT x FROM b")
    assert cur.fetchone() == (2,)
