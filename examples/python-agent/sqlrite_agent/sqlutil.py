"""SQL-literal helpers.

The SQLRite Python SDK does not support prepared-statement parameter
binding yet (deferred to engine Phase 5a.2 — see ``sdk/python/README.md``).
Every value must therefore be inlined into the SQL string. These helpers
keep that inlining safe and consistent across the agent.
"""

from __future__ import annotations

from typing import Iterable


def q(value: object) -> str:
    """Render ``value`` as a SQLRite literal safe for direct inlining.

    Strings get single-quote-escaped (``'`` → ``''``). Numbers and bools
    pass through. ``None`` becomes ``NULL``. Lists / tuples of floats
    become the ``[x, y, z]`` syntax SQLRite uses for VECTOR literals.
    """
    if value is None:
        return "NULL"
    if isinstance(value, bool):
        # Cover bool before int — bool is a subclass of int.
        return "1" if value else "0"
    if isinstance(value, (int, float)):
        return repr(value)
    if isinstance(value, (list, tuple)):
        return vec_literal(value)
    if isinstance(value, str):
        escaped = value.replace("'", "''")
        return f"'{escaped}'"
    raise TypeError(f"cannot inline {type(value).__name__} into SQL")


def vec_literal(vec: Iterable[float]) -> str:
    """Render a vector as the SQLRite ``[v1, v2, ...]`` literal."""
    parts = ", ".join(f"{float(x):.6f}" for x in vec)
    return f"[{parts}]"
