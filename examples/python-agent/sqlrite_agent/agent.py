"""Top-level orchestrator: assemble prompts, call the LLM, persist turns."""

from __future__ import annotations

import time
from dataclasses import dataclass
from typing import Optional

from sqlrite_agent.chat import ChatProvider
from sqlrite_agent.memory import Memory, Recall
from sqlrite_agent.db import Fact, Message, Summary

SYSTEM_RULES = """You are a helpful assistant with persistent memory. \
Every turn you receive a "Memory" block recalled from a SQLRite database \
of past conversations. Treat the structured facts in that block as \
authoritative; treat summaries and recalled messages as context to \
inform your reply. When the user asks a question that the memory can \
answer, use the memory — do not say "I don't remember" if the answer \
is right there. Reply concisely."""


@dataclass(frozen=True)
class Turn:
    """One round-trip: user message in, assistant reply out, plus recall."""

    user_message: str
    assistant_reply: str
    recall: Recall


class ChatAgent:
    def __init__(
        self,
        *,
        memory: Memory,
        chat: ChatProvider,
        conversation_id: str = "default",
        recent_window: int = 6,
    ) -> None:
        self.memory = memory
        self.chat = chat
        self.conversation_id = conversation_id
        self.recent_window = recent_window

    # ------------------------------------------------------------------
    # Turn loop

    def turn(self, user_input: str) -> Turn:
        recall = self.memory.recall(user_input, conversation_id=self.conversation_id)
        recent = self.memory.recent(
            conversation_id=self.conversation_id, limit=self.recent_window
        )

        system = self._assemble_system(recall)
        messages = self._assemble_messages(recent, user_input)
        reply = self.chat.complete(system=system, messages=messages)

        self.memory.log_message(
            conversation_id=self.conversation_id,
            role="user",
            content=user_input,
        )
        self.memory.log_message(
            conversation_id=self.conversation_id,
            role="assistant",
            content=reply,
            extract_user_facts=False,
        )

        return Turn(user_message=user_input, assistant_reply=reply, recall=recall)

    # ------------------------------------------------------------------
    # Summarization (manual; the README documents this as a known
    # simplification — automatic eviction would belong in v2).

    def summarize_window(
        self, *, last_n: int = 20, summarizer: Optional[ChatProvider] = None
    ) -> Optional[str]:
        """Summarize the most recent ``last_n`` turns and write to ``summaries``.

        Uses ``self.chat`` to do the summarization unless ``summarizer``
        is passed in (handy for tests). Returns the summary text, or
        ``None`` if there's nothing to summarize.
        """
        recent = self.memory.recent(
            conversation_id=self.conversation_id, limit=last_n
        )
        if not recent:
            return None

        chat = summarizer or self.chat
        transcript = "\n".join(f"{m.role}: {m.content}" for m in recent)
        prompt_messages = [
            {
                "role": "user",
                "content": (
                    "Summarize the following conversation in 3-5 sentences. "
                    "Preserve concrete facts (names, places, preferences, dates). "
                    "Write in third person ('the user', 'the assistant').\n\n"
                    f"{transcript}"
                ),
            }
        ]
        summary = chat.complete(
            system="You are a precise note-taker.",
            messages=prompt_messages,
        )
        if not summary.strip():
            return None

        self.memory.log_summary(
            conversation_id=self.conversation_id,
            start_ts=recent[0].ts,
            end_ts=recent[-1].ts,
            content=summary,
        )
        return summary

    # ------------------------------------------------------------------
    # Internals

    def _assemble_system(self, recall: Recall) -> str:
        sections: list[str] = [SYSTEM_RULES.strip(), ""]

        if recall.facts:
            sections.append("# Known facts (from past conversations)")
            for f in recall.facts:
                sections.append(f"- {f.subject}.{f.predicate} = {f.object}")
            sections.append("")

        if recall.summaries:
            sections.append("# Summaries of older context")
            for s in recall.summaries:
                ts = _fmt_ts(s.end_ts)
                sections.append(f"- ({ts}) {s.content}")
            sections.append("")

        if recall.messages:
            sections.append("# Relevant past messages")
            for m in recall.messages:
                ts = _fmt_ts(m.ts)
                preview = m.content.strip().replace("\n", " ")
                if len(preview) > 280:
                    preview = preview[:277] + "..."
                sections.append(f"- ({ts}) {m.role}: {preview}")
            sections.append("")

        return "\n".join(sections).strip()

    def _assemble_messages(
        self, recent: list[Message], current_user_input: str
    ) -> list[dict[str, str]]:
        out: list[dict[str, str]] = []
        for m in recent:
            if m.role not in ("user", "assistant"):
                continue
            out.append({"role": m.role, "content": m.content})
        out.append({"role": "user", "content": current_user_input})
        return out


def _fmt_ts(ts: int) -> str:
    return time.strftime("%Y-%m-%d %H:%M", time.localtime(ts))
