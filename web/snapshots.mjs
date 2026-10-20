// Snapshots taken when the ARM is released (src/snap.rs), kept in this
// browser's IndexedDB under a digest of everything the machine was built
// from, so the next power-on with the same settings skips the firmware's boot.

const DB = "pimu-snapshots";
const STORE = "snapshots";

function open() {
  return new Promise((resolve, reject) => {
    const request = indexedDB.open(DB, 1);
    request.onupgradeneeded = () => request.result.createObjectStore(STORE);
    request.onsuccess = () => resolve(request.result);
    request.onerror = () => reject(request.error);
  });
}

async function transact(mode, act) {
  const db = await open();
  try {
    return await new Promise((resolve, reject) => {
      const tx = db.transaction(STORE, mode);
      const request = act(tx.objectStore(STORE));
      tx.oncomplete = () => resolve(request.result);
      tx.onerror = () => reject(tx.error);
    });
  } finally {
    db.close();
  }
}

export async function getSnapshot(key) {
  try {
    return (await transact("readonly", (s) => s.get(key))) ?? null;
  } catch {
    return null;
  }
}

export async function putSnapshot(key, bytes) {
  try {
    await transact("readwrite", (s) => s.put({ bytes, saved: Date.now() }, key));
    return true;
  } catch {
    return false;
  }
}

// Files stand in by name, size and date: their bytes are the user's, and a
// file picked again is the same file as far as a snapshot is concerned.
function describe(value) {
  if (value instanceof Blob) return { file: value.name ?? "", size: value.size, modified: value.lastModified ?? 0 };
  if (value instanceof Uint8Array) return { bytes: value.length };
  if (Array.isArray(value)) return value.map(describe);
  if (value && typeof value === "object") {
    return Object.fromEntries(Object.entries(value).sort(([a], [b]) => (a < b ? -1 : 1)).map(([k, v]) => [k, describe(v)]));
  }
  return value;
}

// What the machine is built from, less what only the ARM side or the log
// depends on: a snapshot is taken before either matters.
export async function snapshotKey(request) {
  const { parallel, log, snapshot, ...options } = request.options;
  const eeprom = new Uint8Array(await crypto.subtle.digest("SHA-256", request.eeprom));
  const what = JSON.stringify(describe({ ...request, options, eeprom: [...eeprom], type: undefined }));
  const digest = new Uint8Array(await crypto.subtle.digest("SHA-256", new TextEncoder().encode(what)));
  return [...digest].map((b) => b.toString(16).padStart(2, "0")).join("");
}
