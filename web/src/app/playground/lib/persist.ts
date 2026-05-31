// Browser persistence for the playground.
//
// The WASM SDK is in-memory only — there is no `.sqlrite` byte image to
// stash (see the playground README's "Known limitations"). So instead of
// persisting the *database*, we persist the *script*: the ordered list of
// mutating statements (CREATE / INSERT / UPDATE / DELETE / tx control) that
// the user has successfully run. On reload we replay that log into a fresh
// in-memory DB, which reconstructs the same state. This is the "script
// replay" model — cheap, transparent, and downloadable as a plain `.sql`.
//
// Storage backend, in priority order:
//   1. OPFS (Origin Private File System) — the modern browser-local FS.
//   2. localStorage — fallback for browsers without OPFS write support.
//   3. none — private-mode / locked-down contexts; the playground still
//      works, it just won't survive a reload.

export type StorageMode = "opfs" | "local" | "none";

const LOG_FILE = "session.sql";
const EDITOR_FILE = "editor.sql";
const LOCAL_PREFIX = "sqlrite-playground:";

type KV = {
  mode: StorageMode;
  get(name: string): Promise<string | null>;
  set(name: string, value: string): Promise<void>;
  remove(name: string): Promise<void>;
};

async function getOpfsDir(): Promise<FileSystemDirectoryHandle | null> {
  try {
    if (
      typeof navigator === "undefined" ||
      !navigator.storage ||
      typeof navigator.storage.getDirectory !== "function"
    ) {
      return null;
    }
    const root = await navigator.storage.getDirectory();
    // Probe that createWritable exists — Safari shipped getDirectory before
    // main-thread writable streams, so a successful getDirectory doesn't
    // guarantee we can write. Create + write + delete a probe file.
    const probe = await root.getFileHandle("__probe__", { create: true });
    if (typeof probe.createWritable !== "function") return null;
    const w = await probe.createWritable();
    await w.write("ok");
    await w.close();
    await root.removeEntry("__probe__").catch(() => {});
    return root;
  } catch {
    return null;
  }
}

function localKV(): KV {
  return {
    mode: "local",
    async get(name) {
      try {
        return localStorage.getItem(LOCAL_PREFIX + name);
      } catch {
        return null;
      }
    },
    async set(name, value) {
      try {
        localStorage.setItem(LOCAL_PREFIX + name, value);
      } catch {
        /* quota / disabled — best effort */
      }
    },
    async remove(name) {
      try {
        localStorage.removeItem(LOCAL_PREFIX + name);
      } catch {
        /* ignore */
      }
    },
  };
}

function noneKV(): KV {
  return {
    mode: "none",
    async get() {
      return null;
    },
    async set() {},
    async remove() {},
  };
}

function opfsKV(root: FileSystemDirectoryHandle): KV {
  return {
    mode: "opfs",
    async get(name) {
      try {
        const fh = await root.getFileHandle(name);
        const file = await fh.getFile();
        return await file.text();
      } catch {
        return null; // ENOENT etc.
      }
    },
    async set(name, value) {
      const fh = await root.getFileHandle(name, { create: true });
      const w = await fh.createWritable();
      await w.write(value);
      await w.close();
    },
    async remove(name) {
      await root.removeEntry(name).catch(() => {});
    },
  };
}

let kvPromise: Promise<KV> | null = null;

async function getKV(): Promise<KV> {
  if (!kvPromise) {
    kvPromise = (async () => {
      const root = await getOpfsDir();
      if (root) return opfsKV(root);
      // localStorage availability probe.
      try {
        const k = `${LOCAL_PREFIX}__probe__`;
        localStorage.setItem(k, "1");
        localStorage.removeItem(k);
        return localKV();
      } catch {
        return noneKV();
      }
    })();
  }
  return kvPromise;
}

/** Which backend persistence resolved to. Awaitable; cached after first call. */
export async function storageMode(): Promise<StorageMode> {
  return (await getKV()).mode;
}

/** The replayable mutating-statement log (newline-joined `.sql`), or null. */
export async function loadSession(): Promise<string | null> {
  return (await getKV()).get(LOG_FILE);
}

export async function saveSession(sql: string): Promise<void> {
  return (await getKV()).set(LOG_FILE, sql);
}

/** Last editor contents — restored on reload when there's no share hash. */
export async function loadEditor(): Promise<string | null> {
  return (await getKV()).get(EDITOR_FILE);
}

export async function saveEditor(sql: string): Promise<void> {
  return (await getKV()).set(EDITOR_FILE, sql);
}

/** Wipes both the session log and the saved editor (the "Reset DB" path). */
export async function clearAll(): Promise<void> {
  const kv = await getKV();
  await kv.remove(LOG_FILE);
  await kv.remove(EDITOR_FILE);
}

// ---------------------------------------------------------------------------
// Share-via-URL-hash. The editor SQL is base64url-encoded into
// `#sql=…` so a link reproduces exactly what someone typed.

function toBase64Url(text: string): string {
  const bytes = new TextEncoder().encode(text);
  let bin = "";
  for (const b of bytes) bin += String.fromCharCode(b);
  return btoa(bin).replace(/\+/g, "-").replace(/\//g, "_").replace(/=+$/, "");
}

function fromBase64Url(b64url: string): string {
  const b64 = b64url.replace(/-/g, "+").replace(/_/g, "/");
  const bin = atob(b64);
  const bytes = new Uint8Array(bin.length);
  for (let i = 0; i < bin.length; i++) bytes[i] = bin.charCodeAt(i);
  return new TextDecoder().decode(bytes);
}

/** Builds a shareable absolute URL with the SQL encoded in the hash. */
export function buildShareUrl(sql: string): string {
  const base = `${window.location.origin}${window.location.pathname}`;
  return `${base}#sql=${toBase64Url(sql)}`;
}

/** Extracts SQL from a `#sql=…` hash, or null if absent / malformed. */
export function readShareHash(hash: string): string | null {
  const m = /[#&]sql=([^&]+)/.exec(hash);
  if (!m) return null;
  try {
    return fromBase64Url(decodeURIComponent(m[1]));
  } catch {
    return null;
  }
}
