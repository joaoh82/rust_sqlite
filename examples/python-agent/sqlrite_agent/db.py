"""SQLRite-backed storage for the chat agent.

Owns: schema migrations, embedding dimension, all SQL — every other module
calls into ``Memory`` (in ``memory.py``) rather than touching SQL directly.
"""

from __future__ import annotations

import time
from contextlib import contextmanager
from dataclasses import dataclass
from typing import Iterator, Optional

import sqlrite

from sqlrite_agent.sqlutil import q

# Vector dimension — fixed at agent boot. Must match whatever your
# embedder produces. 384 is a common sentence-transformer default and
# what the SQLR-39 ticket sketched out.
DEFAULT_DIM = 384
SCHEMA_VERSION = 1


@dataclass(frozen=True)
class Message:
    id: int
    conversation_id: str
    role: str
    content: str
    ts: int


@dataclass(frozen=True)
class Summary:
    id: int
    conversation_id: str
    start_ts: int
    end_ts: int
    content: str


@dataclass(frozen=True)
class Fact:
    id: int
    subject: str
    predicate: str
    object: str
    source_message_id: Optional[int]
    ts: int


class AgentDB:
    """Thin wrapper around a SQLRite ``Connection`` with the agent's schema."""

    def __init__(self, path: str, *, dim: int = DEFAULT_DIM) -> None:
        self.path = path
        self.dim = dim
        self._conn = sqlrite.connect(path)
        self._migrate()

    # ------------------------------------------------------------------
    # Migrations

    def _migrate(self) -> None:
        cur = self._conn.cursor()
        # The SQLRite engine's `CREATE TABLE IF NOT EXISTS` currently
        # still errors when the table exists; detect a pre-existing
        # schema by trying to read the version table directly.
        try:
            cur.execute("SELECT version FROM schema_version")
            row = cur.fetchone()
            current = int(row[0]) if row else 0
        except sqlrite.SQLRiteError:
            cur.execute("CREATE TABLE schema_version (version INTEGER PRIMARY KEY)")
            current = 0

        if current < 1:
            self._apply_v1()
            cur.execute(f"INSERT INTO schema_version (version) VALUES ({SCHEMA_VERSION})")

    def _apply_v1(self) -> None:
        cur = self._conn.cursor()
        dim = self.dim
        cur.execute(
            f"""
            CREATE TABLE messages (
                id INTEGER PRIMARY KEY,
                conversation_id TEXT NOT NULL,
                role TEXT NOT NULL,
                content TEXT NOT NULL,
                ts INTEGER NOT NULL,
                embedding VECTOR({dim})
            )
            """
        )
        cur.execute(
            f"""
            CREATE TABLE summaries (
                id INTEGER PRIMARY KEY,
                conversation_id TEXT NOT NULL,
                start_ts INTEGER NOT NULL,
                end_ts INTEGER NOT NULL,
                content TEXT NOT NULL,
                embedding VECTOR({dim})
            )
            """
        )
        cur.execute(
            """
            CREATE TABLE facts (
                id INTEGER PRIMARY KEY,
                subject TEXT NOT NULL,
                predicate TEXT NOT NULL,
                object TEXT NOT NULL,
                source_message_id INTEGER,
                ts INTEGER NOT NULL
            )
            """
        )
        # HNSW indexes — kick in automatically when the executor sees
        # ORDER BY vec_distance_*(embedding, [...]) LIMIT k.
        cur.execute("CREATE INDEX idx_messages_emb ON messages USING hnsw (embedding)")
        cur.execute("CREATE INDEX idx_summaries_emb ON summaries USING hnsw (embedding)")
        # FTS / BM25 inverted indexes — kick in automatically when the
        # executor sees WHERE fts_match(content, 'q') ORDER BY
        # bm25_score(content, 'q') DESC LIMIT k. Phase 8 (engine).
        cur.execute("CREATE INDEX idx_messages_fts ON messages USING fts (content)")
        cur.execute("CREATE INDEX idx_summaries_fts ON summaries USING fts (content)")

    # ------------------------------------------------------------------
    # Writes

    def insert_message(
        self,
        *,
        conversation_id: str,
        role: str,
        content: str,
        embedding: list[float],
        ts: Optional[int] = None,
    ) -> int:
        ts = ts or int(time.time())
        sql = (
            "INSERT INTO messages (conversation_id, role, content, ts, embedding) "
            f"VALUES ({q(conversation_id)}, {q(role)}, {q(content)}, {q(ts)}, {q(embedding)})"
        )
        cur = self._conn.cursor()
        cur.execute(sql)
        return self._last_rowid("messages")

    def insert_summary(
        self,
        *,
        conversation_id: str,
        start_ts: int,
        end_ts: int,
        content: str,
        embedding: list[float],
    ) -> int:
        sql = (
            "INSERT INTO summaries (conversation_id, start_ts, end_ts, content, embedding) "
            f"VALUES ({q(conversation_id)}, {q(start_ts)}, {q(end_ts)}, {q(content)}, {q(embedding)})"
        )
        cur = self._conn.cursor()
        cur.execute(sql)
        return self._last_rowid("summaries")

    def insert_fact(
        self,
        *,
        subject: str,
        predicate: str,
        object_: str,
        source_message_id: Optional[int] = None,
        ts: Optional[int] = None,
    ) -> int:
        ts = ts or int(time.time())
        src = q(source_message_id) if source_message_id is not None else "NULL"
        sql = (
            "INSERT INTO facts (subject, predicate, object, source_message_id, ts) "
            f"VALUES ({q(subject)}, {q(predicate)}, {q(object_)}, {src}, {q(ts)})"
        )
        cur = self._conn.cursor()
        cur.execute(sql)
        return self._last_rowid("facts")

    def _last_rowid(self, table: str) -> int:
        cur = self._conn.cursor()
        cur.execute(f"SELECT id FROM {table} ORDER BY id DESC LIMIT 1")
        row = cur.fetchone()
        return int(row[0]) if row else -1

    # ------------------------------------------------------------------
    # Reads

    def recent_messages(self, *, conversation_id: str, limit: int) -> list[Message]:
        sql = (
            "SELECT id, conversation_id, role, content, ts FROM messages "
            f"WHERE conversation_id = {q(conversation_id)} "
            f"ORDER BY id DESC LIMIT {int(limit)}"
        )
        cur = self._conn.cursor()
        cur.execute(sql)
        rows = list(cur.fetchall())
        rows.reverse()  # chronological
        return [Message(*r) for r in rows]

    def vector_search_messages(
        self,
        *,
        embedding: list[float],
        k: int,
        conversation_id: Optional[str] = None,
    ) -> list[Message]:
        """Top-k messages by cosine distance to ``embedding``.

        ``conversation_id`` filters by conversation if provided. Without
        it, we search the entire memory (useful for cross-session recall).
        """
        where = (
            f"WHERE conversation_id = {q(conversation_id)} "
            if conversation_id is not None
            else ""
        )
        sql = (
            "SELECT id, conversation_id, role, content, ts FROM messages "
            f"{where}"
            f"ORDER BY vec_distance_cosine(embedding, {q(embedding)}) "
            f"LIMIT {int(k)}"
        )
        cur = self._conn.cursor()
        cur.execute(sql)
        return [Message(*r) for r in cur.fetchall()]

    def vector_search_summaries(
        self,
        *,
        embedding: list[float],
        k: int,
        conversation_id: Optional[str] = None,
    ) -> list[Summary]:
        where = (
            f"WHERE conversation_id = {q(conversation_id)} "
            if conversation_id is not None
            else ""
        )
        sql = (
            "SELECT id, conversation_id, start_ts, end_ts, content FROM summaries "
            f"{where}"
            f"ORDER BY vec_distance_cosine(embedding, {q(embedding)}) "
            f"LIMIT {int(k)}"
        )
        cur = self._conn.cursor()
        cur.execute(sql)
        return [Summary(*r) for r in cur.fetchall()]

    def lexical_search_messages(
        self,
        *,
        keywords: list[str],
        k: int,
        conversation_id: Optional[str] = None,
    ) -> list[Message]:
        """BM25-ranked recall over messages.

        Builds an any-term FTS query from ``keywords`` and lets the
        engine's Phase 8 ``try_fts_probe`` optimizer hook serve top-k
        from the inverted index in O(query-terms × k log k). A
        ``conversation_id`` filter, if provided, is applied as a
        scalar post-filter (see ``docs/fts.md`` — "filtered FTS").
        """
        if not keywords:
            return []
        query = " ".join(keywords)
        conv_clause = (
            f"AND conversation_id = {q(conversation_id)} " if conversation_id else ""
        )
        sql = (
            "SELECT id, conversation_id, role, content, ts FROM messages "
            f"WHERE fts_match(content, {q(query)}) {conv_clause}"
            f"ORDER BY bm25_score(content, {q(query)}) DESC LIMIT {int(k)}"
        )
        cur = self._conn.cursor()
        cur.execute(sql)
        return [Message(*r) for r in cur.fetchall()]

    def search_facts(self, *, keywords: list[str], k: int = 20) -> list[Fact]:
        if not keywords:
            return []
        keyword_clauses = " OR ".join(
            f"subject LIKE {q('%' + kw + '%')} OR "
            f"predicate LIKE {q('%' + kw + '%')} OR "
            f"object LIKE {q('%' + kw + '%')}"
            for kw in keywords
        )
        sql = (
            "SELECT id, subject, predicate, object, source_message_id, ts FROM facts "
            f"WHERE {keyword_clauses} "
            f"ORDER BY ts DESC LIMIT {int(k)}"
        )
        cur = self._conn.cursor()
        cur.execute(sql)
        return [Fact(*r) for r in cur.fetchall()]

    def all_facts(self, limit: int = 100) -> list[Fact]:
        sql = (
            "SELECT id, subject, predicate, object, source_message_id, ts FROM facts "
            f"ORDER BY ts DESC LIMIT {int(limit)}"
        )
        cur = self._conn.cursor()
        cur.execute(sql)
        return [Fact(*r) for r in cur.fetchall()]

    def messages_in_window(
        self, *, conversation_id: str, start_ts: int, end_ts: int
    ) -> list[Message]:
        sql = (
            "SELECT id, conversation_id, role, content, ts FROM messages "
            f"WHERE conversation_id = {q(conversation_id)} "
            f"AND ts >= {q(start_ts)} AND ts <= {q(end_ts)} "
            "ORDER BY id ASC"
        )
        cur = self._conn.cursor()
        cur.execute(sql)
        return [Message(*r) for r in cur.fetchall()]

    def count(self, table: str) -> int:
        cur = self._conn.cursor()
        cur.execute(f"SELECT COUNT(*) FROM {table}")
        row = cur.fetchone()
        return int(row[0]) if row else 0

    # ------------------------------------------------------------------
    # Lifecycle

    def close(self) -> None:
        self._conn.close()

    @contextmanager
    def transaction(self) -> Iterator[None]:
        cur = self._conn.cursor()
        cur.execute("BEGIN")
        try:
            yield
            self._conn.commit()
        except Exception:
            self._conn.rollback()
            raise
