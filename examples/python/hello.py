"""Minimal walkthrough of the SQLRite Python bindings.

Run after:

    cd sdk/python
    maturin develop

    python examples/python/hello.py

The shape mirrors the stdlib `sqlite3` module — if you've used that,
you already know how to drive this.
"""

import sqlrite


def main() -> None:
    # Use `:memory:` for a transient in-memory DB (matching sqlite3
    # convention); pass a path like "foo.sqlrite" for a file-backed
    # one that auto-saves on every write.
    with sqlrite.connect(":memory:") as conn:
        cur = conn.cursor()

        cur.execute(
            "CREATE TABLE users (id INTEGER PRIMARY KEY, name TEXT, age INTEGER)"
        )
        cur.execute("INSERT INTO users (name, age) VALUES ('alice', 30)")
        cur.execute("INSERT INTO users (name, age) VALUES ('bob', 25)")
        cur.execute("INSERT INTO users (name, age) VALUES ('carol', 40)")

        # `.description` exposes PEP 249 column metadata — a list of
        # 7-tuples, name in position 0, rest None until we track
        # types.
        cur.execute("SELECT id, name, age FROM users")
        print("Columns:", [col[0] for col in cur.description])

        # Iterate tuples DB-API-style.
        print("\nAll users:")
        for row in cur:
            uid, name, age = row
            print(f"  {uid}: {name} ({age})")

        # Transactions: BEGIN + INSERT + ROLLBACK leaves the table
        # unchanged. (`commit()` / `rollback()` work on the
        # Connection; the `with` block auto-commits on clean exit
        # and rolls back on exception.)
        cur.execute("BEGIN")
        cur.execute("INSERT INTO users (name, age) VALUES ('phantom', 99)")
        cur.execute("SELECT id FROM users")
        print(f"\nMid-transaction row count: {len(cur.fetchall())}")

        conn.rollback()
        cur.execute("SELECT id FROM users")
        print(f"Post-rollback row count:   {len(cur.fetchall())}")


if __name__ == "__main__":
    main()
