"""Interactive CLI: ``python -m sqlrite_agent`` or ``sqlrite-agent``."""

from __future__ import annotations

import argparse
import os
import sys
from typing import Optional

from sqlrite_agent import __version__
from sqlrite_agent.agent import ChatAgent
from sqlrite_agent.chat import build_chat
from sqlrite_agent.db import DEFAULT_DIM, AgentDB
from sqlrite_agent.embeddings import build_embedder
from sqlrite_agent.memory import Memory

DEFAULT_DB_PATH = os.path.expanduser("~/.sqlrite-agent.sqlrite")


def build_parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(
        prog="sqlrite-agent",
        description=(
            "A Python CLI chat agent with long-term memory backed by SQLRite. "
            "Vector recall over past turns + summaries, plus structured fact "
            "extraction. The entire memory layer is one file on disk."
        ),
    )
    parser.add_argument(
        "--db",
        default=DEFAULT_DB_PATH,
        help=f"path to the SQLRite memory file (default: {DEFAULT_DB_PATH})",
    )
    parser.add_argument(
        "--conversation",
        default="default",
        help="conversation id (lets one DB host multiple parallel threads)",
    )
    parser.add_argument(
        "--embedder",
        choices=["hash", "openai", "local"],
        default="hash",
        help=(
            "embedding provider. 'hash' is the zero-dep default; swap to "
            "'openai' or 'local' (sentence-transformers) for real semantic recall."
        ),
    )
    parser.add_argument(
        "--chat",
        choices=["auto", "anthropic", "echo"],
        default="auto",
        help=(
            "LLM provider. 'auto' picks anthropic if ANTHROPIC_API_KEY is set, "
            "else 'echo' (offline, canned replies)."
        ),
    )
    parser.add_argument("--version", action="version", version=f"%(prog)s {__version__}")
    return parser


def main(argv: Optional[list[str]] = None) -> int:
    args = build_parser().parse_args(argv)

    embedder = build_embedder(args.embedder, dim=DEFAULT_DIM)
    db = AgentDB(args.db, dim=embedder.dim)
    memory = Memory(db, embedder)
    chat = build_chat(args.chat)
    agent = ChatAgent(memory=memory, chat=chat, conversation_id=args.conversation)

    _print_banner(args, chat.__class__.__name__, embedder.__class__.__name__, memory)

    try:
        return _repl(agent, memory)
    finally:
        db.close()


def _repl(agent: ChatAgent, memory: Memory) -> int:
    while True:
        try:
            user_input = input("you> ").strip()
        except (EOFError, KeyboardInterrupt):
            print()
            return 0
        if not user_input:
            continue

        if user_input.startswith("/"):
            if _handle_slash(user_input, agent, memory):
                continue
            return 0

        turn = agent.turn(user_input)
        if turn.recall.facts or turn.recall.messages or turn.recall.summaries:
            print(
                f"  [recalled: {len(turn.recall.facts)} facts, "
                f"{len(turn.recall.summaries)} summaries, "
                f"{len(turn.recall.messages)} messages]"
            )
        print(f"agent> {turn.assistant_reply}\n")


def _handle_slash(cmd: str, agent: ChatAgent, memory: Memory) -> bool:
    """Returns True to keep the REPL running, False to exit."""
    parts = cmd[1:].split(maxsplit=1)
    name = parts[0].lower() if parts else ""
    arg = parts[1] if len(parts) > 1 else ""

    if name in ("quit", "exit", "q"):
        return False

    if name == "help":
        _print_help()
        return True

    if name == "stats":
        s = memory.stats()
        print(
            f"  messages={s['messages']}, summaries={s['summaries']}, "
            f"facts={s['facts']}"
        )
        return True

    if name == "facts":
        rows = memory.all_facts(limit=50)
        if not rows:
            print("  (no facts extracted yet)")
        for f in rows:
            print(f"  {f.subject}.{f.predicate} = {f.object}")
        return True

    if name == "recent":
        rows = memory.recent(conversation_id=agent.conversation_id, limit=10)
        for m in rows:
            preview = m.content.replace("\n", " ")
            if len(preview) > 100:
                preview = preview[:97] + "..."
            print(f"  [{m.id}] {m.role}: {preview}")
        return True

    if name == "recall":
        if not arg:
            print("  usage: /recall <query>")
            return True
        r = memory.recall(arg, conversation_id=agent.conversation_id)
        print(f"  facts: {len(r.facts)}, summaries: {len(r.summaries)}, messages: {len(r.messages)}")
        for f in r.facts[:5]:
            print(f"    fact: {f.subject}.{f.predicate} = {f.object}")
        for m in r.messages[:5]:
            preview = m.content.replace("\n", " ")
            if len(preview) > 100:
                preview = preview[:97] + "..."
            print(f"    msg [{m.id}] {m.role}: {preview}")
        return True

    if name == "summarize":
        summary = agent.summarize_window()
        if summary:
            print(f"  summary written:\n  {summary}")
        else:
            print("  (nothing to summarize)")
        return True

    print(f"  unknown command: /{name}. Try /help.")
    return True


def _print_banner(args, chat_cls: str, emb_cls: str, memory: Memory) -> None:
    s = memory.stats()
    print(
        f"sqlrite-agent {__version__} — db={args.db}, "
        f"conversation={args.conversation}, embedder={emb_cls}, chat={chat_cls}"
    )
    print(
        f"  loaded memory: {s['messages']} messages, "
        f"{s['summaries']} summaries, {s['facts']} facts. "
        "Type /help for commands, Ctrl-D to quit."
    )


def _print_help() -> None:
    print(
        """  /help               this message
  /stats              counts of messages, summaries, facts
  /facts              list all extracted facts
  /recent             last 10 turns (chronological)
  /recall <query>     show what would be recalled for a query, without replying
  /summarize          summarize the last 20 turns and store the summary
  /quit               exit (Ctrl-D also works)"""
    )


if __name__ == "__main__":
    sys.exit(main())
