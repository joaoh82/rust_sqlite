// Runtime loader for the SQLRite WASM SDK.
//
// The pkg is wasm-pack's `--target web` output, vendored into
// `web/public/playground/pkg/` (a pinned copy of `sdk/wasm/pkg/` — see the
// playground README). We load it with a *runtime* dynamic import of the
// public path rather than letting the Next bundler process it: wasm-pack's
// glue uses `import.meta.url` + top-level `fetch` of the sibling `.wasm`,
// which webpack's wasm handling mangles. The `webpackIgnore` /
// `turbopackIgnore` magic comments keep the import as a native browser
// `import()` of `/playground/pkg/sqlrite_wasm.js`, so the glue resolves the
// `.wasm` next to itself exactly as wasm-pack intends. There is no module
// on disk for TypeScript to resolve, so the types are declared here to
// mirror `sdk/wasm/pkg/sqlrite_wasm.d.ts`.

/** A single result row: column-name → typed JS primitive. */
export type SqlriteRow = Record<string, string | number | boolean | null>;

/**
 * In-memory SQLRite database handle. The WASM build is always in-memory —
 * there is no file-backed or serialised mode (see the playground README's
 * "Known limitations").
 */
export interface Database {
  /** CREATE / INSERT / UPDATE / DELETE / BEGIN / COMMIT / ROLLBACK. */
  exec(sql: string): void;
  /** A SELECT — array of row objects in projection order. */
  query(sql: string): SqlriteRow[];
  /** Column names of a SELECT's projection, without iterating rows. */
  columns(sql: string): string[];
  /** Releases the underlying Rust state. */
  free(): void;
  readonly inTransaction: boolean;
  readonly readonly: boolean;
}

type WasmModule = {
  default: (module_or_path?: unknown) => Promise<unknown>;
  Database: new () => Database;
};

let modPromise: Promise<WasmModule> | null = null;

/**
 * Loads + initialises the WASM module once per page. Subsequent calls
 * return the same in-flight / settled promise, so the ~750 KB `.wasm`
 * is fetched and instantiated exactly once.
 */
export async function loadSqlrite(): Promise<{ Database: new () => Database }> {
  if (!modPromise) {
    modPromise = (async () => {
      // @ts-expect-error — runtime ESM import resolved from web/public in the
      // browser; not a module the bundler/TS can see on disk. The magic
      // comments keep webpack/turbopack from trying to bundle it.
      const mod = (await import(/* webpackIgnore: true */ /* turbopackIgnore: true */ "/playground/pkg/sqlrite_wasm.js")) as unknown as WasmModule;
      // wasm-bindgen start: fetch + instantiate the binary. Everything
      // throws until this resolves.
      await mod.default();
      return mod;
    })();
  }
  return modPromise;
}
