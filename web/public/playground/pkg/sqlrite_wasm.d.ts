/* tslint:disable */
/* eslint-disable */

export class AskPromptOptions {
    free(): void;
    [Symbol.dispose](): void;
    /**
     * Construct an empty options object. JS can mutate fields in
     * place: `const opts = new AskPromptOptions(); opts.model = '...'`.
     */
    constructor();
    /**
     * Anthropic prompt-cache TTL on the schema block: `"5m"`
     * (default), `"1h"`, or `"off"`.
     */
    get cache_ttl(): string | undefined;
    /**
     * Anthropic prompt-cache TTL on the schema block: `"5m"`
     * (default), `"1h"`, or `"off"`.
     */
    set cache_ttl(value: string | null | undefined);
    /**
     * `max_tokens` for the LLM call (default: 1024).
     */
    get max_tokens(): number | undefined;
    /**
     * `max_tokens` for the LLM call (default: 1024).
     */
    set max_tokens(value: number | null | undefined);
    /**
     * Model ID (default: `"claude-sonnet-4-6"`).
     */
    get model(): string | undefined;
    /**
     * Model ID (default: `"claude-sonnet-4-6"`).
     */
    set model(value: string | null | undefined);
}

/**
 * A SQLRite database handle. Always in-memory in the WASM build.
 * Drop the handle (set to `null` / let GC collect it) to free the
 * underlying state.
 */
export class Database {
    free(): void;
    [Symbol.dispose](): void;
    /**
     * Parse an Anthropic API response (the full JSON the JS caller's
     * fetch returned) back into `{ sql, explanation, usage }`.
     *
     * Pass the raw response body as a string. The parser:
     *   * Extracts the first text content block from `content[]`.
     *   * Reads token counts from `usage`.
     *   * Parses the model's text as JSON (tolerant to fenced /
     *     leading-prose shapes — see `sqlrite_ask::parse_response`).
     *
     * On parse failure (the model emitted unparseable text, or the
     * API response was malformed), throws a JS Error with the
     * underlying reason.
     */
    askParse(raw_api_response: string): any;
    /**
     * Build the LLM-provider request payload for `question` against
     * the current schema. Returns a JS object ready to POST to the
     * caller's backend.
     *
     * ```js
     * const payload = db.askPrompt('How many users?');
     * // → { model, max_tokens, system: [...], messages: [...] }
     * const response = await fetch('/api/llm/complete', {
     *   method: 'POST',
     *   body: JSON.stringify(payload),
     * });
     * const apiResponse = await response.json();
     * const result = db.askParse(JSON.stringify(apiResponse));
     * // → { sql, explanation, usage: {...} }
     * ```
     *
     * `options` (optional) accepts:
     *   * `model` — override the default `claude-sonnet-4-6`.
     *   * `maxTokens` — override the default `1024`.
     *   * `cacheTtl` — `"5m"` (default), `"1h"`, or `"off"`.
     */
    askPrompt(question: string, options?: AskPromptOptions | null): any;
    /**
     * Number of columns in the projection of a SELECT. Useful when
     * a caller wants to build their own UI column list without
     * iterating rows.
     */
    columns(sql: string): any;
    /**
     * Runs one SQL statement that doesn't produce rows (CREATE /
     * INSERT / UPDATE / DELETE / BEGIN / COMMIT / ROLLBACK). For
     * SELECT use [`query`].
     */
    exec(sql: string): void;
    /**
     * Creates an in-memory database. The only mode supported by
     * the WASM build — file-backed mode isn't meaningful in a
     * browser sandbox.
     */
    constructor();
    /**
     * Runs a SELECT and returns an array of row objects. Each
     * object's keys are column names in projection order; values
     * are typed JS primitives — `number` for Integer/Real,
     * `string` for Text, `boolean` for Bool, `null` for NULL.
     */
    query(sql: string): any;
    /**
     * Returns `true` while a `BEGIN … COMMIT/ROLLBACK` block is open.
     */
    readonly inTransaction: boolean;
    /**
     * Always `false` in the WASM build (file-backed / read-only
     * opens aren't exposed). Kept for API-shape parity with the
     * Node.js SDK.
     */
    readonly readonly: boolean;
}

/**
 * Runs once when the WASM module is first imported. Wires up
 * `console.error`-backed panic reporting so a Rust panic shows a
 * real stack trace in devtools instead of a generic "unreachable"
 * trap.
 */
export function _init(): void;

export type InitInput = RequestInfo | URL | Response | BufferSource | WebAssembly.Module;

export interface InitOutput {
    readonly memory: WebAssembly.Memory;
    readonly __wbg_askpromptoptions_free: (a: number, b: number) => void;
    readonly __wbg_database_free: (a: number, b: number) => void;
    readonly __wbg_get_askpromptoptions_cache_ttl: (a: number) => [number, number];
    readonly __wbg_get_askpromptoptions_max_tokens: (a: number) => number;
    readonly __wbg_get_askpromptoptions_model: (a: number) => [number, number];
    readonly __wbg_set_askpromptoptions_cache_ttl: (a: number, b: number, c: number) => void;
    readonly __wbg_set_askpromptoptions_max_tokens: (a: number, b: number) => void;
    readonly __wbg_set_askpromptoptions_model: (a: number, b: number, c: number) => void;
    readonly _init: () => void;
    readonly askpromptoptions_new: () => number;
    readonly database_askParse: (a: number, b: number, c: number) => [number, number, number];
    readonly database_askPrompt: (a: number, b: number, c: number, d: number) => [number, number, number];
    readonly database_columns: (a: number, b: number, c: number) => [number, number, number];
    readonly database_exec: (a: number, b: number, c: number) => [number, number];
    readonly database_inTransaction: (a: number) => number;
    readonly database_new: () => [number, number, number];
    readonly database_query: (a: number, b: number, c: number) => [number, number, number];
    readonly database_readonly: (a: number) => number;
    readonly __wbindgen_malloc: (a: number, b: number) => number;
    readonly __wbindgen_realloc: (a: number, b: number, c: number, d: number) => number;
    readonly __wbindgen_free: (a: number, b: number, c: number) => void;
    readonly __wbindgen_externrefs: WebAssembly.Table;
    readonly __externref_table_dealloc: (a: number) => void;
    readonly __wbindgen_start: () => void;
}

export type SyncInitInput = BufferSource | WebAssembly.Module;

/**
 * Instantiates the given `module`, which can either be bytes or
 * a precompiled `WebAssembly.Module`.
 *
 * @param {{ module: SyncInitInput }} module - Passing `SyncInitInput` directly is deprecated.
 *
 * @returns {InitOutput}
 */
export function initSync(module: { module: SyncInitInput } | SyncInitInput): InitOutput;

/**
 * If `module_or_path` is {RequestInfo} or {URL}, makes a request and
 * for everything else, calls `WebAssembly.instantiate` directly.
 *
 * @param {{ module_or_path: InitInput | Promise<InitInput> }} module_or_path - Passing `InitInput` directly is deprecated.
 *
 * @returns {Promise<InitOutput>}
 */
export default function __wbg_init (module_or_path?: { module_or_path: InitInput | Promise<InitInput> } | InitInput | Promise<InitInput>): Promise<InitOutput>;
