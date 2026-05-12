"""Memory: store messages and recall them by hybrid (vector + lexical) search.

This is the only module the chat loop calls directly. Everything else
in this package is plumbing.
"""

from __future__ import annotations

import re
import time
from dataclasses import dataclass
from typing import Optional

from sqlrite_agent.db import AgentDB, Fact, Message, Summary
from sqlrite_agent.embeddings import Embedder
from sqlrite_agent.facts import extract_facts


@dataclass(frozen=True)
class Recall:
    """What the agent recalled in response to a query.

    The three buckets serve different roles in the prompt:

    * ``facts`` — deterministic, exact recall from structured triples.
      Always trustworthy because it came from earlier user statements
      via the regex extractor.
    * ``summaries`` — periodic rollups of older turns. Wide context,
      lossy. Good for "what was this conversation about" recall.
    * ``messages`` — individual past turns ranked by hybrid similarity.
      High precision, narrow context.
    """

    facts: list[Fact]
    summaries: list[Summary]
    messages: list[Message]


_KEYWORD_RE = re.compile(r"[A-Za-z][A-Za-z0-9]{2,}")
_STOP = frozenset(
    {
        "the", "and", "for", "with", "you", "your", "this", "that", "from",
        "are", "was", "were", "have", "has", "had", "but", "not", "what",
        "when", "where", "which", "who", "why", "how", "into", "about",
        "did", "does", "doing", "done", "been", "being", "than", "then",
    }
)


def query_keywords(text: str, *, limit: int = 6) -> list[str]:
    """Extract content keywords from a query for the lexical recall step."""
    seen: set[str] = set()
    out: list[str] = []
    for tok in _KEYWORD_RE.findall(text.lower()):
        if tok in _STOP or tok in seen:
            continue
        seen.add(tok)
        out.append(tok)
        if len(out) >= limit:
            break
    return out


class Memory:
    """High-level operations on the agent's persistent memory."""

    def __init__(self, db: AgentDB, embedder: Embedder) -> None:
        if embedder.dim != db.dim:
            raise ValueError(
                f"embedder dim {embedder.dim} does not match db dim {db.dim} "
                f"(file format pins the schema's VECTOR({db.dim}))"
            )
        self._db = db
        self._embedder = embedder

    # ------------------------------------------------------------------
    # Writes

    def log_message(
        self,
        *,
        conversation_id: str,
        role: str,
        content: str,
        extract_user_facts: bool = True,
    ) -> int:
        """Embed and persist a chat turn.

        For user messages, also runs the heuristic fact extractor and
        writes any extracted triples — wired in here (rather than at the
        call site) so callers can't forget.
        """
        embedding = self._embedder.embed(content)
        ts = int(time.time())
        msg_id = self._db.insert_message(
            conversation_id=conversation_id,
            role=role,
            content=content,
            embedding=embedding,
            ts=ts,
        )
        if extract_user_facts and role == "user":
            for fact in extract_facts(content):
                self._db.insert_fact(
                    subject=fact.subject,
                    predicate=fact.predicate,
                    object_=fact.object,
                    source_message_id=msg_id,
                    ts=ts,
                )
        return msg_id

    def log_summary(
        self,
        *,
        conversation_id: str,
        start_ts: int,
        end_ts: int,
        content: str,
    ) -> int:
        embedding = self._embedder.embed(content)
        return self._db.insert_summary(
            conversation_id=conversation_id,
            start_ts=start_ts,
            end_ts=end_ts,
            content=content,
            embedding=embedding,
        )

    # ------------------------------------------------------------------
    # Reads

    def recall(
        self,
        query: str,
        *,
        conversation_id: Optional[str] = None,
        k_messages: int = 4,
        k_summaries: int = 2,
        k_facts: int = 10,
    ) -> Recall:
        """Hybrid recall for ``query``.

        Strategy:
        1. Embed the query and pull top-k messages + summaries by cosine
           distance (vector / semantic half).
        2. Extract keywords and pull additional messages via LIKE (lexical
           fallback for Phase 8 BM25, which isn't shipped yet).
        3. Pull keyword-matched facts directly from the structured table.
        4. Merge and dedupe by id, preserving the vector-search ranking
           and appending lexical-only hits afterward.
        """
        embedding = self._embedder.embed(query)
        keywords = query_keywords(query)

        vec_hits = self._db.vector_search_messages(
            embedding=embedding,
            k=k_messages,
            conversation_id=conversation_id,
        )
        lex_hits = self._db.lexical_search_messages(
            keywords=keywords,
            k=k_messages,
            conversation_id=conversation_id,
        )

        messages = _merge_ranked(vec_hits, lex_hits, key=lambda m: m.id)[:k_messages * 2]

        summaries = self._db.vector_search_summaries(
            embedding=embedding,
            k=k_summaries,
            conversation_id=conversation_id,
        )
        facts = self._db.search_facts(keywords=keywords, k=k_facts)

        return Recall(facts=facts, summaries=summaries, messages=messages)

    def recent(self, *, conversation_id: str, limit: int = 6) -> list[Message]:
        return self._db.recent_messages(conversation_id=conversation_id, limit=limit)

    def messages_in_window(
        self, *, conversation_id: str, start_ts: int, end_ts: int
    ) -> list[Message]:
        return self._db.messages_in_window(
            conversation_id=conversation_id,
            start_ts=start_ts,
            end_ts=end_ts,
        )

    def all_facts(self, limit: int = 100) -> list[Fact]:
        return self._db.all_facts(limit=limit)

    def stats(self) -> dict[str, int]:
        return {
            "messages": self._db.count("messages"),
            "summaries": self._db.count("summaries"),
            "facts": self._db.count("facts"),
        }


def _merge_ranked(primary, secondary, *, key):
    seen: set = set()
    out = []
    for item in primary:
        k = key(item)
        if k in seen:
            continue
        seen.add(k)
        out.append(item)
    for item in secondary:
        k = key(item)
        if k in seen:
            continue
        seen.add(k)
        out.append(item)
    return out
