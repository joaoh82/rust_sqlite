"""Embedding providers.

Three implementations:

* :class:`HashEmbedder` — deterministic hash-bag-of-words. No deps, no
  API key. The default so the example runs end-to-end on a fresh
  machine. Semantic quality is mediocre; good enough for the demo's
  10-turn conversation, not good enough for real RAG.
* :class:`OpenAIEmbedder` — ``text-embedding-3-small`` with explicit
  ``dimensions=384`` so it matches the schema.
* :class:`LocalEmbedder` — sentence-transformers (``all-MiniLM-L6-v2``,
  natively 384 dims). Best zero-key option for real semantic recall,
  at the cost of ~500 MB of torch.

Pick one at agent boot. All three implement :class:`Embedder`.
"""

from __future__ import annotations

import hashlib
import math
import os
import re
from typing import Protocol

DEFAULT_DIM = 384
_TOKEN_RE = re.compile(r"[A-Za-z0-9]+")


class Embedder(Protocol):
    dim: int

    def embed(self, text: str) -> list[float]: ...


# ---------------------------------------------------------------------------
# Hash bag-of-words — the zero-dependency default.


class HashEmbedder:
    """Token-hash → fixed-dim vector.

    Each token's MD5 picks a bucket; we increment that bucket. The
    final vector is L2-normalized so cosine distance is meaningful.
    Two texts sharing tokens end up with overlapping non-zero buckets
    and a small cosine distance.

    This is a placeholder for real embeddings. Swap in OpenAI or
    sentence-transformers for production.
    """

    def __init__(self, dim: int = DEFAULT_DIM) -> None:
        self.dim = dim

    def embed(self, text: str) -> list[float]:
        vec = [0.0] * self.dim
        tokens = _TOKEN_RE.findall(text.lower())
        if not tokens:
            return vec
        for tok in tokens:
            h = hashlib.md5(tok.encode("utf-8")).digest()
            # First 4 bytes → bucket; next byte's sign bit → sign.
            bucket = int.from_bytes(h[:4], "little") % self.dim
            sign = 1.0 if (h[4] & 1) == 0 else -1.0
            vec[bucket] += sign
        norm = math.sqrt(sum(v * v for v in vec))
        if norm > 0:
            vec = [v / norm for v in vec]
        return vec


# ---------------------------------------------------------------------------
# OpenAI — text-embedding-3-small with dimensions=384.


class OpenAIEmbedder:
    """``text-embedding-3-small`` constrained to ``dim`` dimensions."""

    def __init__(self, *, dim: int = DEFAULT_DIM, api_key: str | None = None) -> None:
        try:
            from openai import OpenAI  # type: ignore[import-not-found]
        except ImportError as e:  # pragma: no cover - import guard
            raise RuntimeError(
                "install the 'openai' extra to use OpenAIEmbedder: "
                "`pip install 'sqlrite-agent[openai]'`"
            ) from e

        self.dim = dim
        self._OpenAI = OpenAI
        self._client = OpenAI(api_key=api_key or os.environ.get("OPENAI_API_KEY"))

    def embed(self, text: str) -> list[float]:
        resp = self._client.embeddings.create(
            model="text-embedding-3-small",
            input=text,
            dimensions=self.dim,
        )
        return list(resp.data[0].embedding)


# ---------------------------------------------------------------------------
# sentence-transformers — local, no API key.


class LocalEmbedder:
    """sentence-transformers ``all-MiniLM-L6-v2`` (384-dim by default)."""

    def __init__(self, *, model_name: str = "sentence-transformers/all-MiniLM-L6-v2") -> None:
        try:
            from sentence_transformers import SentenceTransformer  # type: ignore[import-not-found]
        except ImportError as e:  # pragma: no cover - import guard
            raise RuntimeError(
                "install the 'local-embeddings' extra to use LocalEmbedder: "
                "`pip install 'sqlrite-agent[local-embeddings]'`"
            ) from e

        self._model = SentenceTransformer(model_name)
        self.dim = self._model.get_sentence_embedding_dimension()

    def embed(self, text: str) -> list[float]:
        return [float(x) for x in self._model.encode(text, normalize_embeddings=True)]


# ---------------------------------------------------------------------------
# Factory


def build_embedder(name: str, *, dim: int = DEFAULT_DIM) -> Embedder:
    """Build an embedder from a short string name.

    Names: ``hash``, ``openai``, ``local``. Raises ``ValueError`` for
    anything else — callers should validate before calling.
    """
    name = name.lower()
    if name == "hash":
        return HashEmbedder(dim=dim)
    if name == "openai":
        return OpenAIEmbedder(dim=dim)
    if name == "local":
        return LocalEmbedder()
    raise ValueError(f"unknown embedder: {name!r} (expected 'hash', 'openai', or 'local')")
