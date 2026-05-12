"""Memory-layer tests: recall combines vector + lexical + facts."""

from __future__ import annotations

from sqlrite_agent.memory import Memory, query_keywords


def test_keywords_filter_stop_words():
    kws = query_keywords("what is the weather like in lisbon today")
    assert "lisbon" in kws
    assert "the" not in kws
    assert "what" not in kws


def test_log_user_message_extracts_facts(memory: Memory):
    memory.log_message(
        conversation_id="c1",
        role="user",
        content="My dog's name is Mochi.",
    )
    facts = memory.all_facts()
    assert any(
        f.subject == "user.dog" and f.object == "Mochi" for f in facts
    )


def test_log_assistant_does_not_extract_facts(memory: Memory):
    memory.log_message(
        conversation_id="c1",
        role="assistant",
        content="Your dog's name is Mochi.",  # assistant echoing back
    )
    assert memory.all_facts() == []


def test_recall_pulls_messages_summaries_and_facts(memory: Memory):
    memory.log_message(
        conversation_id="c1",
        role="user",
        content="My dog's name is Mochi.",
    )
    memory.log_message(
        conversation_id="c1",
        role="user",
        content="Mochi loves carrots more than treats.",
    )
    memory.log_message(
        conversation_id="c1",
        role="user",
        content="The weather in Lisbon is sunny today.",
    )

    r = memory.recall("what does mochi like to eat", conversation_id="c1")
    assert any("Mochi" in f.object for f in r.facts)
    assert any("mochi" in m.content.lower() for m in r.messages)


def test_recall_works_without_conversation_filter(memory: Memory):
    memory.log_message(conversation_id="c1", role="user", content="apples are red")
    memory.log_message(conversation_id="c2", role="user", content="bananas are yellow")
    r = memory.recall("color of an apple")
    contents = " ".join(m.content for m in r.messages)
    assert "apple" in contents


def test_recall_persists_across_reopen(tmp_path, embedder):
    from sqlrite_agent.db import AgentDB

    path = str(tmp_path / "mem.sqlrite")
    db = AgentDB(path)
    mem = Memory(db, embedder)
    mem.log_message(
        conversation_id="c1",
        role="user",
        content="My favorite color is blue.",
    )
    db.close()

    db2 = AgentDB(path)
    mem2 = Memory(db2, embedder)
    r = mem2.recall("what color do i like", conversation_id="c1")
    assert any(
        f.predicate == "favorite_color" and f.object == "blue" for f in r.facts
    )
    db2.close()
