"""Top-level agent tests: turn loop, prompt assembly, summarization."""

from __future__ import annotations

from sqlrite_agent.agent import ChatAgent
from sqlrite_agent.chat import ChatProvider


class CapturingChat:
    """A test double that records what the agent asked it."""

    def __init__(self, reply: str = "ok") -> None:
        self.reply = reply
        self.last_system: str = ""
        self.last_messages: list[dict[str, str]] = []

    def complete(self, *, system: str, messages: list[dict[str, str]]) -> str:
        self.last_system = system
        self.last_messages = list(messages)
        return self.reply


def test_turn_writes_user_and_assistant_messages(memory):
    chat: ChatProvider = CapturingChat(reply="hi there")
    agent = ChatAgent(memory=memory, chat=chat, conversation_id="c1")

    turn = agent.turn("Hello!")
    assert turn.assistant_reply == "hi there"

    s = memory.stats()
    assert s["messages"] == 2  # one user, one assistant


def test_turn_assembles_system_prompt_with_recalled_facts(memory):
    # Seed a known fact, then issue a related query in a NEW turn.
    memory.log_message(
        conversation_id="c1",
        role="user",
        content="My dog's name is Mochi.",
    )

    chat = CapturingChat(reply="cool")
    agent = ChatAgent(memory=memory, chat=chat, conversation_id="c1")
    agent.turn("Tell me about Mochi")

    assert "user.dog.name = Mochi" in chat.last_system


def test_turn_includes_recent_chat_history(memory):
    chat = CapturingChat(reply="ok")
    agent = ChatAgent(memory=memory, chat=chat, conversation_id="c1")

    agent.turn("first message")
    agent.turn("second message")

    # Third turn should see both prior turns in the message list.
    agent.turn("third message")
    user_contents = [m["content"] for m in chat.last_messages if m["role"] == "user"]
    assert "first message" in user_contents
    assert "second message" in user_contents
    assert "third message" in user_contents


def test_summarize_window_writes_a_summary(memory):
    chat = CapturingChat(reply="The user talked about their dog and the weather.")
    agent = ChatAgent(memory=memory, chat=chat, conversation_id="c1")
    agent.turn("My dog's name is Mochi.")
    agent.turn("The weather in Lisbon is sunny.")

    summary = agent.summarize_window(last_n=10)
    assert summary is not None
    assert memory.stats()["summaries"] == 1


def test_recall_survives_db_reopen(tmp_path, embedder):
    """The headline demo: memory across process restarts."""
    from sqlrite_agent.db import AgentDB
    from sqlrite_agent.memory import Memory

    path = str(tmp_path / "agent.sqlrite")

    # --- session 1 ---
    db = AgentDB(path)
    memory = Memory(db, embedder)
    chat = CapturingChat(reply="ok")
    agent = ChatAgent(memory=memory, chat=chat, conversation_id="c1")
    agent.turn("My dog's name is Mochi.")
    agent.turn("Mochi loves carrots.")
    db.close()

    # --- session 2: fresh process, same DB ---
    db = AgentDB(path)
    memory = Memory(db, embedder)
    chat = CapturingChat(reply="great")
    agent = ChatAgent(memory=memory, chat=chat, conversation_id="c1")

    agent.turn("What does Mochi eat?")
    # The system prompt for the new turn should contain the fact we
    # stored in session 1. That's the entire point of the demo.
    assert "Mochi" in chat.last_system
