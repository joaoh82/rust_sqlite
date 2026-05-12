"""Lightweight fact extraction.

Pulls structured ``(subject, predicate, object)`` triples out of user
messages with regex heuristics. Deliberately conservative — we'd
rather miss a fact than hallucinate one. The agent surfaces facts via
plain SQL on the ``facts`` table, so a few well-chosen patterns beat
calling an LLM on every turn.

Production agents would call the LLM here; the demo keeps it offline
so the example runs without an API key.
"""

from __future__ import annotations

import re
from dataclasses import dataclass


@dataclass(frozen=True)
class ExtractedFact:
    subject: str
    predicate: str
    object: str


# (regex, fact-builder) pairs. The builder receives the regex match
# and returns an ``ExtractedFact``. Order matters — first match wins
# for a given sentence.
_PATTERNS: list[tuple[re.Pattern[str], "callable"]] = [  # type: ignore[type-arg]
    # "my dog's name is Mochi" / "my dog is called Mochi"
    (
        re.compile(
            r"\bmy\s+([a-zA-Z][a-zA-Z\s]{0,40}?)(?:'s)?\s+(?:name\s+is|is\s+called)\s+([A-Z][\w'-]{1,40})",
            re.IGNORECASE,
        ),
        lambda m: ExtractedFact(
            subject=f"user.{_slug(m.group(1))}",
            predicate="name",
            object=m.group(2),
        ),
    ),
    # "I live in <City>" / "I'm from <City>"
    (
        re.compile(
            r"\bI(?:\s+am|'m)?\s+(?:live\s+in|from|based\s+in)\s+([A-Z][\w\s,]{1,60})",
            re.IGNORECASE,
        ),
        lambda m: ExtractedFact(
            subject="user",
            predicate="location",
            object=_clean(m.group(1)),
        ),
    ),
    # "I work as a <role>" / "I'm a <role>"
    (
        re.compile(
            r"\bI(?:\s+am|'m)?\s+(?:work\s+as\s+a|a)\s+([a-zA-Z][a-zA-Z\s]{1,40})",
            re.IGNORECASE,
        ),
        lambda m: ExtractedFact(
            subject="user",
            predicate="role",
            object=_clean(m.group(1)),
        ),
    ),
    # "My favorite <X> is <Y>"
    (
        re.compile(
            r"\bmy\s+favou?rite\s+([a-zA-Z][a-zA-Z\s]{1,30})\s+is\s+([\w\s'-]{1,60})",
            re.IGNORECASE,
        ),
        lambda m: ExtractedFact(
            subject="user",
            predicate=f"favorite_{_slug(m.group(1))}",
            object=_clean(m.group(2)),
        ),
    ),
    # "I like <X>" / "I love <X>"
    (
        re.compile(
            r"\bI\s+(?:like|love|enjoy)\s+([\w\s'-]{1,60})",
            re.IGNORECASE,
        ),
        lambda m: ExtractedFact(
            subject="user",
            predicate="likes",
            object=_clean(m.group(1)),
        ),
    ),
    # "I have a <X> named <Y>"
    (
        re.compile(
            r"\bI\s+have\s+a\s+([a-zA-Z][a-zA-Z\s]{1,30})\s+(?:named|called)\s+([A-Z][\w'-]{1,40})",
            re.IGNORECASE,
        ),
        lambda m: ExtractedFact(
            subject=f"user.{_slug(m.group(1))}",
            predicate="name",
            object=m.group(2),
        ),
    ),
]


def extract_facts(text: str) -> list[ExtractedFact]:
    """Pull every distinct fact out of ``text``.

    Splits on sentence boundaries, runs each pattern, deduplicates.
    """
    facts: list[ExtractedFact] = []
    seen: set[tuple[str, str, str]] = set()
    for sentence in _sentences(text):
        for pattern, builder in _PATTERNS:
            m = pattern.search(sentence)
            if not m:
                continue
            fact = builder(m)
            key = (fact.subject, fact.predicate, fact.object)
            if key in seen:
                continue
            seen.add(key)
            facts.append(fact)
            # One fact per sentence to keep the heuristics tractable.
            break
    return facts


def _sentences(text: str) -> list[str]:
    return [s.strip() for s in re.split(r"(?<=[.!?])\s+|\n", text) if s.strip()]


def _slug(s: str) -> str:
    return re.sub(r"[^a-z0-9]+", "_", s.strip().lower()).strip("_")


def _clean(s: str) -> str:
    return s.strip().rstrip(".,!?;:'\" ")
