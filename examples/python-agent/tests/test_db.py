"""DB-layer tests: schema, inserts, vector recall, lexical fallback."""

from __future__ import annotations

import time

from sqlrite_agent.db import AgentDB


def test_schema_creates_three_tables_and_records_version(db: AgentDB):
    cur = db._conn.cursor()  # noqa: SLF001 — test reaches into internals
    cur.execute("SELECT version FROM schema_version")
    row = cur.fetchone()
    assert row == (1,)


def test_insert_and_fetch_message(db: AgentDB, embedder):
    msg_id = db.insert_message(
        conversation_id="c1",
        role="user",
        content="hello world",
        embedding=embedder.embed("hello world"),
    )
    assert msg_id >= 1

    rows = db.recent_messages(conversation_id="c1", limit=5)
    assert len(rows) == 1
    assert rows[0].role == "user"
    assert rows[0].content == "hello world"


def test_insert_handles_single_quotes_in_content(db: AgentDB, embedder):
    # The agent must survive any user content. This is the SQL-injection
    # smoke test for the q() inlining helper.
    payload = "I'm here; '); DROP TABLE messages; --"
    db.insert_message(
        conversation_id="c1",
        role="user",
        content=payload,
        embedding=embedder.embed(payload),
    )
    rows = db.recent_messages(conversation_id="c1", limit=5)
    assert rows[0].content == payload
    # Schema is intact.
    cur = db._conn.cursor()  # noqa: SLF001
    cur.execute("SELECT COUNT(*) FROM messages")
    assert cur.fetchone() == (1,)


def test_vector_search_orders_by_cosine_distance(db: AgentDB, embedder):
    db.insert_message(
        conversation_id="c1",
        role="user",
        content="my dog mochi loves carrots",
        embedding=embedder.embed("my dog mochi loves carrots"),
    )
    db.insert_message(
        conversation_id="c1",
        role="user",
        content="the weather in lisbon is sunny today",
        embedding=embedder.embed("the weather in lisbon is sunny today"),
    )

    query = "what does mochi like to eat"
    hits = db.vector_search_messages(
        embedding=embedder.embed(query), k=2, conversation_id="c1"
    )
    assert len(hits) == 2
    # The mochi/carrots row shares tokens with the query, so it should
    # rank above the weather/lisbon row under our hash embedder.
    assert "mochi" in hits[0].content


def test_lexical_search_messages(db: AgentDB, embedder):
    db.insert_message(
        conversation_id="c1",
        role="user",
        content="alice loves running",
        embedding=embedder.embed("alice loves running"),
    )
    db.insert_message(
        conversation_id="c1",
        role="user",
        content="bob plays the piano",
        embedding=embedder.embed("bob plays the piano"),
    )
    hits = db.lexical_search_messages(
        keywords=["alice"], k=10, conversation_id="c1"
    )
    assert len(hits) == 1
    assert "alice" in hits[0].content


def test_lexical_search_ranks_by_bm25(db: AgentDB, embedder):
    # Two rows share the term 'database'; only one shares 'embedded'.
    # BM25 should put the row with more matching terms (and rarer ones)
    # ahead of the row with just one common-ish term.
    for body in (
        "redis is an in-memory database that caches values",
        "sqlrite is an embedded database engine",
        "postgres is a relational database server",
        "rust is a systems programming language",
    ):
        db.insert_message(
            conversation_id="c1",
            role="user",
            content=body,
            embedding=embedder.embed(body),
        )

    hits = db.lexical_search_messages(
        keywords=["embedded", "database"], k=10, conversation_id="c1"
    )
    assert hits, "FTS should find at least one match"
    assert "embedded database" in hits[0].content


def test_lexical_search_handles_unmatched_query(db: AgentDB, embedder):
    db.insert_message(
        conversation_id="c1",
        role="user",
        content="alice loves running",
        embedding=embedder.embed("alice loves running"),
    )
    # Query terms that aren't in any document — fts_match returns no
    # rows, which the agent must tolerate (vector recall still runs).
    hits = db.lexical_search_messages(
        keywords=["nonexistentterm"], k=10, conversation_id="c1"
    )
    assert hits == []


def test_facts_round_trip(db: AgentDB):
    db.insert_fact(subject="user.dog", predicate="name", object_="Mochi")
    db.insert_fact(subject="user", predicate="location", object_="Lisbon")
    found = db.search_facts(keywords=["mochi"])
    assert len(found) == 1
    assert found[0].subject == "user.dog"


def test_messages_in_window(db: AgentDB, embedder):
    base = int(time.time())
    for i in range(5):
        db.insert_message(
            conversation_id="c1",
            role="user",
            content=f"msg {i}",
            embedding=embedder.embed(f"msg {i}"),
            ts=base + i,
        )
    window = db.messages_in_window(
        conversation_id="c1", start_ts=base + 1, end_ts=base + 3
    )
    assert [m.content for m in window] == ["msg 1", "msg 2", "msg 3"]


def test_persists_across_reopen(tmp_path, embedder):
    path = str(tmp_path / "agent.sqlrite")
    db = AgentDB(path)
    db.insert_message(
        conversation_id="c1",
        role="user",
        content="i live in lisbon",
        embedding=embedder.embed("i live in lisbon"),
    )
    db.close()

    db2 = AgentDB(path)
    rows = db2.recent_messages(conversation_id="c1", limit=5)
    assert len(rows) == 1
    assert rows[0].content == "i live in lisbon"
    db2.close()
