"""Chat (LLM) providers.

Two implementations:

* :class:`AnthropicChat` — the default. Reads ``ANTHROPIC_API_KEY``
  from the environment.
* :class:`EchoChat` — deterministic offline fake. Echoes recalled
  context back; used by the test suite and as the fallback when no
  API key is configured so ``python -m sqlrite_agent`` runs end-to-end
  on a fresh machine without surprises.

Both implement :class:`ChatProvider`.
"""

from __future__ import annotations

import os
from typing import Protocol


class ChatProvider(Protocol):
    """Single-shot completion given a system prompt + a message list."""

    def complete(self, *, system: str, messages: list[dict[str, str]]) -> str: ...


# ---------------------------------------------------------------------------
# Anthropic — default provider.


class AnthropicChat:
    """Claude via the ``anthropic`` SDK."""

    def __init__(
        self,
        *,
        api_key: str | None = None,
        model: str = "claude-haiku-4-5",
        max_tokens: int = 512,
    ) -> None:
        try:
            from anthropic import Anthropic  # type: ignore[import-not-found]
        except ImportError as e:  # pragma: no cover - import guard
            raise RuntimeError(
                "install the 'anthropic' extra to use AnthropicChat: "
                "`pip install 'sqlrite-agent[anthropic]'`"
            ) from e

        self.model = model
        self.max_tokens = max_tokens
        self._client = Anthropic(api_key=api_key or os.environ.get("ANTHROPIC_API_KEY"))

    def complete(self, *, system: str, messages: list[dict[str, str]]) -> str:
        resp = self._client.messages.create(
            model=self.model,
            max_tokens=self.max_tokens,
            system=system,
            messages=messages,
        )
        out: list[str] = []
        for block in resp.content:
            text = getattr(block, "text", None)
            if text:
                out.append(text)
        return "".join(out).strip()


# ---------------------------------------------------------------------------
# Echo — deterministic, offline.


class EchoChat:
    """A stand-in for an LLM that returns the system prompt + last turn.

    Useful for two things:

    1. Tests — completion output is deterministic.
    2. Zero-key first-run — users without an API key can still see the
       recall pipeline work end to end. The "agent" replies are obviously
       canned, but the prompt assembly is real.
    """

    def complete(self, *, system: str, messages: list[dict[str, str]]) -> str:
        last_user = next(
            (m["content"] for m in reversed(messages) if m.get("role") == "user"),
            "",
        )
        return (
            "[echo agent — no LLM configured; set ANTHROPIC_API_KEY for real replies]\n"
            f"I heard: {last_user!r}\n"
            "(The system prompt recalled context above this line — that's the part "
            "this example is showing off. The reply itself is canned.)"
        )


# ---------------------------------------------------------------------------
# Factory


def build_chat(name: str | None) -> ChatProvider:
    """Pick a provider from ``name``.

    Names: ``anthropic``, ``echo``, or ``auto`` (default). ``auto``
    picks Anthropic if ``ANTHROPIC_API_KEY`` is set, otherwise Echo.
    """
    if not name or name == "auto":
        name = "anthropic" if os.environ.get("ANTHROPIC_API_KEY") else "echo"
    name = name.lower()
    if name == "anthropic":
        return AnthropicChat()
    if name == "echo":
        return EchoChat()
    raise ValueError(f"unknown chat provider: {name!r}")
