// A static server for the page: the tests never build the WebAssembly, so this
// serves web/ as it is and answers 404 for what is missing (pkg/ included).
//
// It answers HEAD and `Range` requests like a server that hosts disk images
// does, and `?norange=1` plays one that does not.

import { createReadStream, existsSync, statSync } from "node:fs";
import { createServer } from "node:http";
import { extname, join, normalize } from "node:path";
import { fileURLToPath } from "node:url";

const root = fileURLToPath(new URL("..", import.meta.url));
const types = {
  ".html": "text/html; charset=utf-8",
  ".js": "text/javascript; charset=utf-8",
  ".mjs": "text/javascript; charset=utf-8",
  ".css": "text/css; charset=utf-8",
  ".svg": "image/svg+xml",
  ".wasm": "application/wasm",
};

createServer((request, response) => {
  const url = new URL(request.url, "http://x");
  const path = normalize(decodeURIComponent(url.pathname));
  const file = join(root, path === "/" ? "index.html" : path);
  if (!file.startsWith(root) || file.includes("node_modules") || !existsSync(file) || !statSync(file).isFile()) {
    response.writeHead(404).end("not found");
    return;
  }
  const size = statSync(file).size;
  const headers = {
    "content-type": types[extname(file)] ?? "application/octet-stream",
    "cache-control": "no-store",
    "accept-ranges": "bytes",
  };
  const range = /^bytes=(\d+)-(\d*)$/.exec(request.headers.range ?? "");
  if (request.method === "HEAD") {
    response.writeHead(200, { ...headers, "content-length": size }).end();
  } else if (range && !url.searchParams.has("norange")) {
    const start = Number(range[1]);
    const end = Math.min(range[2] ? Number(range[2]) : size - 1, size - 1);
    response.writeHead(206, { ...headers, "content-length": end - start + 1, "content-range": `bytes ${start}-${end}/${size}` });
    createReadStream(file, { start, end }).pipe(response);
  } else {
    response.writeHead(200, { ...headers, "content-length": size });
    createReadStream(file).pipe(response);
  }
}).listen(4173, "127.0.0.1");
