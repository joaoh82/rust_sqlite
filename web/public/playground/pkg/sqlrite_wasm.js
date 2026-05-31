/* @ts-self-types="./sqlrite_wasm.d.ts" */

export class AskPromptOptions {
    __destroy_into_raw() {
        const ptr = this.__wbg_ptr;
        this.__wbg_ptr = 0;
        AskPromptOptionsFinalization.unregister(this);
        return ptr;
    }
    free() {
        const ptr = this.__destroy_into_raw();
        wasm.__wbg_askpromptoptions_free(ptr, 0);
    }
    /**
     * Construct an empty options object. JS can mutate fields in
     * place: `const opts = new AskPromptOptions(); opts.model = '...'`.
     */
    constructor() {
        const ret = wasm.askpromptoptions_new();
        this.__wbg_ptr = ret >>> 0;
        AskPromptOptionsFinalization.register(this, this.__wbg_ptr, this);
        return this;
    }
    /**
     * Anthropic prompt-cache TTL on the schema block: `"5m"`
     * (default), `"1h"`, or `"off"`.
     * @returns {string | undefined}
     */
    get cache_ttl() {
        const ret = wasm.__wbg_get_askpromptoptions_cache_ttl(this.__wbg_ptr);
        let v1;
        if (ret[0] !== 0) {
            v1 = getStringFromWasm0(ret[0], ret[1]).slice();
            wasm.__wbindgen_free(ret[0], ret[1] * 1, 1);
        }
        return v1;
    }
    /**
     * `max_tokens` for the LLM call (default: 1024).
     * @returns {number | undefined}
     */
    get max_tokens() {
        const ret = wasm.__wbg_get_askpromptoptions_max_tokens(this.__wbg_ptr);
        return ret === 0x100000001 ? undefined : ret;
    }
    /**
     * Model ID (default: `"claude-sonnet-4-6"`).
     * @returns {string | undefined}
     */
    get model() {
        const ret = wasm.__wbg_get_askpromptoptions_model(this.__wbg_ptr);
        let v1;
        if (ret[0] !== 0) {
            v1 = getStringFromWasm0(ret[0], ret[1]).slice();
            wasm.__wbindgen_free(ret[0], ret[1] * 1, 1);
        }
        return v1;
    }
    /**
     * Anthropic prompt-cache TTL on the schema block: `"5m"`
     * (default), `"1h"`, or `"off"`.
     * @param {string | null} [arg0]
     */
    set cache_ttl(arg0) {
        var ptr0 = isLikeNone(arg0) ? 0 : passStringToWasm0(arg0, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
        var len0 = WASM_VECTOR_LEN;
        wasm.__wbg_set_askpromptoptions_cache_ttl(this.__wbg_ptr, ptr0, len0);
    }
    /**
     * `max_tokens` for the LLM call (default: 1024).
     * @param {number | null} [arg0]
     */
    set max_tokens(arg0) {
        wasm.__wbg_set_askpromptoptions_max_tokens(this.__wbg_ptr, isLikeNone(arg0) ? 0x100000001 : (arg0) >>> 0);
    }
    /**
     * Model ID (default: `"claude-sonnet-4-6"`).
     * @param {string | null} [arg0]
     */
    set model(arg0) {
        var ptr0 = isLikeNone(arg0) ? 0 : passStringToWasm0(arg0, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
        var len0 = WASM_VECTOR_LEN;
        wasm.__wbg_set_askpromptoptions_model(this.__wbg_ptr, ptr0, len0);
    }
}
if (Symbol.dispose) AskPromptOptions.prototype[Symbol.dispose] = AskPromptOptions.prototype.free;

/**
 * A SQLRite database handle. Always in-memory in the WASM build.
 * Drop the handle (set to `null` / let GC collect it) to free the
 * underlying state.
 */
export class Database {
    __destroy_into_raw() {
        const ptr = this.__wbg_ptr;
        this.__wbg_ptr = 0;
        DatabaseFinalization.unregister(this);
        return ptr;
    }
    free() {
        const ptr = this.__destroy_into_raw();
        wasm.__wbg_database_free(ptr, 0);
    }
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
     * @param {string} raw_api_response
     * @returns {any}
     */
    askParse(raw_api_response) {
        const ptr0 = passStringToWasm0(raw_api_response, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
        const len0 = WASM_VECTOR_LEN;
        const ret = wasm.database_askParse(this.__wbg_ptr, ptr0, len0);
        if (ret[2]) {
            throw takeFromExternrefTable0(ret[1]);
        }
        return takeFromExternrefTable0(ret[0]);
    }
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
     * @param {string} question
     * @param {AskPromptOptions | null} [options]
     * @returns {any}
     */
    askPrompt(question, options) {
        const ptr0 = passStringToWasm0(question, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
        const len0 = WASM_VECTOR_LEN;
        let ptr1 = 0;
        if (!isLikeNone(options)) {
            _assertClass(options, AskPromptOptions);
            ptr1 = options.__destroy_into_raw();
        }
        const ret = wasm.database_askPrompt(this.__wbg_ptr, ptr0, len0, ptr1);
        if (ret[2]) {
            throw takeFromExternrefTable0(ret[1]);
        }
        return takeFromExternrefTable0(ret[0]);
    }
    /**
     * Number of columns in the projection of a SELECT. Useful when
     * a caller wants to build their own UI column list without
     * iterating rows.
     * @param {string} sql
     * @returns {any}
     */
    columns(sql) {
        const ptr0 = passStringToWasm0(sql, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
        const len0 = WASM_VECTOR_LEN;
        const ret = wasm.database_columns(this.__wbg_ptr, ptr0, len0);
        if (ret[2]) {
            throw takeFromExternrefTable0(ret[1]);
        }
        return takeFromExternrefTable0(ret[0]);
    }
    /**
     * Runs one SQL statement that doesn't produce rows (CREATE /
     * INSERT / UPDATE / DELETE / BEGIN / COMMIT / ROLLBACK). For
     * SELECT use [`query`].
     * @param {string} sql
     */
    exec(sql) {
        const ptr0 = passStringToWasm0(sql, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
        const len0 = WASM_VECTOR_LEN;
        const ret = wasm.database_exec(this.__wbg_ptr, ptr0, len0);
        if (ret[1]) {
            throw takeFromExternrefTable0(ret[0]);
        }
    }
    /**
     * Returns `true` while a `BEGIN … COMMIT/ROLLBACK` block is open.
     * @returns {boolean}
     */
    get inTransaction() {
        const ret = wasm.database_inTransaction(this.__wbg_ptr);
        return ret !== 0;
    }
    /**
     * Creates an in-memory database. The only mode supported by
     * the WASM build — file-backed mode isn't meaningful in a
     * browser sandbox.
     */
    constructor() {
        const ret = wasm.database_new();
        if (ret[2]) {
            throw takeFromExternrefTable0(ret[1]);
        }
        this.__wbg_ptr = ret[0] >>> 0;
        DatabaseFinalization.register(this, this.__wbg_ptr, this);
        return this;
    }
    /**
     * Runs a SELECT and returns an array of row objects. Each
     * object's keys are column names in projection order; values
     * are typed JS primitives — `number` for Integer/Real,
     * `string` for Text, `boolean` for Bool, `null` for NULL.
     * @param {string} sql
     * @returns {any}
     */
    query(sql) {
        const ptr0 = passStringToWasm0(sql, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
        const len0 = WASM_VECTOR_LEN;
        const ret = wasm.database_query(this.__wbg_ptr, ptr0, len0);
        if (ret[2]) {
            throw takeFromExternrefTable0(ret[1]);
        }
        return takeFromExternrefTable0(ret[0]);
    }
    /**
     * Always `false` in the WASM build (file-backed / read-only
     * opens aren't exposed). Kept for API-shape parity with the
     * Node.js SDK.
     * @returns {boolean}
     */
    get readonly() {
        const ret = wasm.database_readonly(this.__wbg_ptr);
        return ret !== 0;
    }
}
if (Symbol.dispose) Database.prototype[Symbol.dispose] = Database.prototype.free;

/**
 * Runs once when the WASM module is first imported. Wires up
 * `console.error`-backed panic reporting so a Rust panic shows a
 * real stack trace in devtools instead of a generic "unreachable"
 * trap.
 */
export function _init() {
    wasm._init();
}
function __wbg_get_imports() {
    const import0 = {
        __proto__: null,
        __wbg_Error_960c155d3d49e4c2: function(arg0, arg1) {
            const ret = Error(getStringFromWasm0(arg0, arg1));
            return ret;
        },
        __wbg_String_8564e559799eccda: function(arg0, arg1) {
            const ret = String(arg1);
            const ptr1 = passStringToWasm0(ret, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
            const len1 = WASM_VECTOR_LEN;
            getDataViewMemory0().setInt32(arg0 + 4 * 1, len1, true);
            getDataViewMemory0().setInt32(arg0 + 4 * 0, ptr1, true);
        },
        __wbg___wbindgen_is_string_6df3bf7ef1164ed3: function(arg0) {
            const ret = typeof(arg0) === 'string';
            return ret;
        },
        __wbg___wbindgen_throw_6b64449b9b9ed33c: function(arg0, arg1) {
            throw new Error(getStringFromWasm0(arg0, arg1));
        },
        __wbg_error_a6fa202b58aa1cd3: function(arg0, arg1) {
            let deferred0_0;
            let deferred0_1;
            try {
                deferred0_0 = arg0;
                deferred0_1 = arg1;
                console.error(getStringFromWasm0(arg0, arg1));
            } finally {
                wasm.__wbindgen_free(deferred0_0, deferred0_1, 1);
            }
        },
        __wbg_new_227d7c05414eb861: function() {
            const ret = new Error();
            return ret;
        },
        __wbg_new_34d45cc8e36aaead: function() {
            const ret = new Map();
            return ret;
        },
        __wbg_new_682678e2f47e32bc: function() {
            const ret = new Array();
            return ret;
        },
        __wbg_new_aa8d0fa9762c29bd: function() {
            const ret = new Object();
            return ret;
        },
        __wbg_set_3bf1de9fab0cd644: function(arg0, arg1, arg2) {
            arg0[arg1 >>> 0] = arg2;
        },
        __wbg_set_6be42768c690e380: function(arg0, arg1, arg2) {
            arg0[arg1] = arg2;
        },
        __wbg_set_fde2cec06c23692b: function(arg0, arg1, arg2) {
            const ret = arg0.set(arg1, arg2);
            return ret;
        },
        __wbg_stack_3b0d974bbf31e44f: function(arg0, arg1) {
            const ret = arg1.stack;
            const ptr1 = passStringToWasm0(ret, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
            const len1 = WASM_VECTOR_LEN;
            getDataViewMemory0().setInt32(arg0 + 4 * 1, len1, true);
            getDataViewMemory0().setInt32(arg0 + 4 * 0, ptr1, true);
        },
        __wbindgen_cast_0000000000000001: function(arg0) {
            // Cast intrinsic for `F64 -> Externref`.
            const ret = arg0;
            return ret;
        },
        __wbindgen_cast_0000000000000002: function(arg0) {
            // Cast intrinsic for `I64 -> Externref`.
            const ret = arg0;
            return ret;
        },
        __wbindgen_cast_0000000000000003: function(arg0, arg1) {
            // Cast intrinsic for `Ref(String) -> Externref`.
            const ret = getStringFromWasm0(arg0, arg1);
            return ret;
        },
        __wbindgen_cast_0000000000000004: function(arg0) {
            // Cast intrinsic for `U64 -> Externref`.
            const ret = BigInt.asUintN(64, arg0);
            return ret;
        },
        __wbindgen_init_externref_table: function() {
            const table = wasm.__wbindgen_externrefs;
            const offset = table.grow(4);
            table.set(0, undefined);
            table.set(offset + 0, undefined);
            table.set(offset + 1, null);
            table.set(offset + 2, true);
            table.set(offset + 3, false);
        },
    };
    return {
        __proto__: null,
        "./sqlrite_wasm_bg.js": import0,
    };
}

const AskPromptOptionsFinalization = (typeof FinalizationRegistry === 'undefined')
    ? { register: () => {}, unregister: () => {} }
    : new FinalizationRegistry(ptr => wasm.__wbg_askpromptoptions_free(ptr >>> 0, 1));
const DatabaseFinalization = (typeof FinalizationRegistry === 'undefined')
    ? { register: () => {}, unregister: () => {} }
    : new FinalizationRegistry(ptr => wasm.__wbg_database_free(ptr >>> 0, 1));

function _assertClass(instance, klass) {
    if (!(instance instanceof klass)) {
        throw new Error(`expected instance of ${klass.name}`);
    }
}

let cachedDataViewMemory0 = null;
function getDataViewMemory0() {
    if (cachedDataViewMemory0 === null || cachedDataViewMemory0.buffer.detached === true || (cachedDataViewMemory0.buffer.detached === undefined && cachedDataViewMemory0.buffer !== wasm.memory.buffer)) {
        cachedDataViewMemory0 = new DataView(wasm.memory.buffer);
    }
    return cachedDataViewMemory0;
}

function getStringFromWasm0(ptr, len) {
    ptr = ptr >>> 0;
    return decodeText(ptr, len);
}

let cachedUint8ArrayMemory0 = null;
function getUint8ArrayMemory0() {
    if (cachedUint8ArrayMemory0 === null || cachedUint8ArrayMemory0.byteLength === 0) {
        cachedUint8ArrayMemory0 = new Uint8Array(wasm.memory.buffer);
    }
    return cachedUint8ArrayMemory0;
}

function isLikeNone(x) {
    return x === undefined || x === null;
}

function passStringToWasm0(arg, malloc, realloc) {
    if (realloc === undefined) {
        const buf = cachedTextEncoder.encode(arg);
        const ptr = malloc(buf.length, 1) >>> 0;
        getUint8ArrayMemory0().subarray(ptr, ptr + buf.length).set(buf);
        WASM_VECTOR_LEN = buf.length;
        return ptr;
    }

    let len = arg.length;
    let ptr = malloc(len, 1) >>> 0;

    const mem = getUint8ArrayMemory0();

    let offset = 0;

    for (; offset < len; offset++) {
        const code = arg.charCodeAt(offset);
        if (code > 0x7F) break;
        mem[ptr + offset] = code;
    }
    if (offset !== len) {
        if (offset !== 0) {
            arg = arg.slice(offset);
        }
        ptr = realloc(ptr, len, len = offset + arg.length * 3, 1) >>> 0;
        const view = getUint8ArrayMemory0().subarray(ptr + offset, ptr + len);
        const ret = cachedTextEncoder.encodeInto(arg, view);

        offset += ret.written;
        ptr = realloc(ptr, len, offset, 1) >>> 0;
    }

    WASM_VECTOR_LEN = offset;
    return ptr;
}

function takeFromExternrefTable0(idx) {
    const value = wasm.__wbindgen_externrefs.get(idx);
    wasm.__externref_table_dealloc(idx);
    return value;
}

let cachedTextDecoder = new TextDecoder('utf-8', { ignoreBOM: true, fatal: true });
cachedTextDecoder.decode();
const MAX_SAFARI_DECODE_BYTES = 2146435072;
let numBytesDecoded = 0;
function decodeText(ptr, len) {
    numBytesDecoded += len;
    if (numBytesDecoded >= MAX_SAFARI_DECODE_BYTES) {
        cachedTextDecoder = new TextDecoder('utf-8', { ignoreBOM: true, fatal: true });
        cachedTextDecoder.decode();
        numBytesDecoded = len;
    }
    return cachedTextDecoder.decode(getUint8ArrayMemory0().subarray(ptr, ptr + len));
}

const cachedTextEncoder = new TextEncoder();

if (!('encodeInto' in cachedTextEncoder)) {
    cachedTextEncoder.encodeInto = function (arg, view) {
        const buf = cachedTextEncoder.encode(arg);
        view.set(buf);
        return {
            read: arg.length,
            written: buf.length
        };
    };
}

let WASM_VECTOR_LEN = 0;

let wasmModule, wasm;
function __wbg_finalize_init(instance, module) {
    wasm = instance.exports;
    wasmModule = module;
    cachedDataViewMemory0 = null;
    cachedUint8ArrayMemory0 = null;
    wasm.__wbindgen_start();
    return wasm;
}

async function __wbg_load(module, imports) {
    if (typeof Response === 'function' && module instanceof Response) {
        if (typeof WebAssembly.instantiateStreaming === 'function') {
            try {
                return await WebAssembly.instantiateStreaming(module, imports);
            } catch (e) {
                const validResponse = module.ok && expectedResponseType(module.type);

                if (validResponse && module.headers.get('Content-Type') !== 'application/wasm') {
                    console.warn("`WebAssembly.instantiateStreaming` failed because your server does not serve Wasm with `application/wasm` MIME type. Falling back to `WebAssembly.instantiate` which is slower. Original error:\n", e);

                } else { throw e; }
            }
        }

        const bytes = await module.arrayBuffer();
        return await WebAssembly.instantiate(bytes, imports);
    } else {
        const instance = await WebAssembly.instantiate(module, imports);

        if (instance instanceof WebAssembly.Instance) {
            return { instance, module };
        } else {
            return instance;
        }
    }

    function expectedResponseType(type) {
        switch (type) {
            case 'basic': case 'cors': case 'default': return true;
        }
        return false;
    }
}

function initSync(module) {
    if (wasm !== undefined) return wasm;


    if (module !== undefined) {
        if (Object.getPrototypeOf(module) === Object.prototype) {
            ({module} = module)
        } else {
            console.warn('using deprecated parameters for `initSync()`; pass a single object instead')
        }
    }

    const imports = __wbg_get_imports();
    if (!(module instanceof WebAssembly.Module)) {
        module = new WebAssembly.Module(module);
    }
    const instance = new WebAssembly.Instance(module, imports);
    return __wbg_finalize_init(instance, module);
}

async function __wbg_init(module_or_path) {
    if (wasm !== undefined) return wasm;


    if (module_or_path !== undefined) {
        if (Object.getPrototypeOf(module_or_path) === Object.prototype) {
            ({module_or_path} = module_or_path)
        } else {
            console.warn('using deprecated parameters for the initialization function; pass a single object instead')
        }
    }

    if (module_or_path === undefined) {
        module_or_path = new URL('sqlrite_wasm_bg.wasm', import.meta.url);
    }
    const imports = __wbg_get_imports();

    if (typeof module_or_path === 'string' || (typeof Request === 'function' && module_or_path instanceof Request) || (typeof URL === 'function' && module_or_path instanceof URL)) {
        module_or_path = fetch(module_or_path);
    }

    const { instance, module } = await __wbg_load(await module_or_path, imports);

    return __wbg_finalize_init(instance, module);
}

export { initSync, __wbg_init as default };
