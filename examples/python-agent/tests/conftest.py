"""Shared pytest fixtures.

Every test runs against a fresh ``AgentDB`` backed by an in-memory
SQLRite database — no temp files, no API keys, no network.
"""

from __future__ import annotations

import pytest

from sqlrite_agent.chat import EchoChat
from sqlrite_agent.db import AgentDB
from sqlrite_agent.embeddings import HashEmbedder
from sqlrite_agent.memory import Memory


@pytest.fixture
def db():
    d = AgentDB(":memory:")
    try:
        yield d
    finally:
        d.close()


@pytest.fixture
def embedder():
    return HashEmbedder()


@pytest.fixture
def memory(db, embedder):
    return Memory(db, embedder)


@pytest.fixture
def chat():
    return EchoChat()
