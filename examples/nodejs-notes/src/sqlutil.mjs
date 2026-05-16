// Tiny SQL-literal helpers — the SQLRite engine doesn't support
// `?`-style parameter binding yet (Phase 5a.2 follow-up), so every
// caller must inline values as SQL literals. This module is the
// single place that does that safely.
//
// Mirrors the shape of `sqlrite_agent.sqlutil` in the Python example.

/**
 * Quote a JavaScript value as a SQL literal.
 *
 * - string  → `'escaped'` (single quotes doubled per the SQL standard)
 * - number  → integer or `Number.prototype.toString()` for finite floats
 * - boolean → `TRUE` / `FALSE`
 * - null/undefined → `NULL`
 * - number[] → `[v1, v2, ...]` — the engine's vector literal syntax
 *
 * Anything else throws — refuse to silently `String()` an object.
 *
 * @param {unknown} value
 * @returns {string}
 */
export function q(value) {
  if (value === null || value === undefined) return 'NULL';
  if (typeof value === 'string') return `'${value.replaceAll("'", "''")}'`;
  if (typeof value === 'number') {
    if (!Number.isFinite(value)) {
      throw new TypeError(`q(): non-finite number ${value}`);
    }
    return Number.isInteger(value) ? String(value) : value.toString();
  }
  if (typeof value === 'bigint') return value.toString();
  if (typeof value === 'boolean') return value ? 'TRUE' : 'FALSE';
  if (Array.isArray(value)) {
    // Vector literal — every element must be finite numeric.
    const parts = value.map((v, i) => {
      if (typeof v !== 'number' || !Number.isFinite(v)) {
        throw new TypeError(`q(): vector element ${i} is not a finite number (got ${v})`);
      }
      // toString() emits the shortest round-trippable form; the
      // engine's parser accepts both fixed-point and exponential.
      return v.toString();
    });
    return `[${parts.join(', ')}]`;
  }
  throw new TypeError(`q(): unsupported value type ${typeof value}`);
}

/**
 * Validate a SQL identifier (table / column / index name) against the
 * unquoted-identifier subset the engine accepts. Throws if invalid.
 *
 * Use this for ANY identifier that ultimately gets inlined into SQL —
 * callers shouldn't have to guess what's safe.
 *
 * @param {string} name
 * @returns {string} the same name (for chaining)
 */
export function ident(name) {
  if (typeof name !== 'string' || !/^[A-Za-z_][A-Za-z0-9_]*$/.test(name)) {
    throw new TypeError(`ident(): invalid SQL identifier ${JSON.stringify(name)}`);
  }
  return name;
}
