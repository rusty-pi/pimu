// A download that says how far it has got.

export async function download(name, url, onProgress) {
  const transfer = { name, url, loaded: 0, total: 0, done: false };
  onProgress(transfer);
  const response = await fetch(url);
  if (!response.ok) throw new Error(`${name}: ${url} answered ${response.status}`);
  transfer.total = Number(response.headers.get("content-length")) || 0;
  const chunks = [];
  const reader = response.body.getReader();
  for (;;) {
    const { done, value } = await reader.read();
    if (done) break;
    chunks.push(value);
    transfer.loaded += value.length;
    onProgress(transfer);
  }
  const bytes = new Uint8Array(transfer.loaded);
  let at = 0;
  for (const chunk of chunks) {
    bytes.set(chunk, at);
    at += chunk.length;
  }
  transfer.total = transfer.loaded;
  transfer.done = true;
  onProgress(transfer, true);
  return bytes;
}

export async function githubListing({ repo, ref, path }) {
  const api = `https://api.github.com/repos/${repo}/contents/${path}?ref=${encodeURIComponent(ref)}`;
  const response = await fetch(api);
  if (!response.ok) throw new Error(`listing ${repo}/${path}@${ref}: GitHub answered ${response.status}`);
  const entries = await response.json();
  if (!Array.isArray(entries)) throw new Error(`${repo}/${path} is not a directory`);
  return entries.filter((e) => e.type === "file").map((e) => ({ path: e.name, url: e.download_url }));
}
