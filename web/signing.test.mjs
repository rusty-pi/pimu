import assert from "node:assert/strict";
import test from "node:test";

import { publicKeyBytes, signImage } from "./signing.mjs";

const bytesOfHex = (text) => Uint8Array.from(text.match(/../g), (h) => parseInt(h, 16));

test("boot.sig is the digest, a timestamp and a signature that the public key checks", async () => {
  const image = new TextEncoder().encode("a boot image");
  const { sig, pubkey } = await signImage(image, 1_792_236_000_999);
  const lines = new TextDecoder().decode(sig).split("\n");
  assert.equal(lines.length, 4);
  assert.equal(lines[3], "");
  const digest = new Uint8Array(await crypto.subtle.digest("SHA-256", image));
  assert.equal(lines[0], [...digest].map((b) => b.toString(16).padStart(2, "0")).join(""));
  assert.equal(lines[1], "ts: 1792236000");
  const signature = lines[2].replace(/^rsa2048: /, "");
  assert.match(signature, /^[0-9a-f]{512}$/);

  // pubkey.bin is the modulus then the exponent, little-endian: read it back as a key
  assert.equal(pubkey.length, 264);
  const n = Buffer.from(pubkey.slice(0, 256).reverse()).toString("base64url");
  const e = Buffer.from(pubkey.slice(256, 259).reverse()).toString("base64url");
  assert.deepEqual([...pubkey.slice(259)], [0, 0, 0, 0, 0]);
  const key = await crypto.subtle.importKey("jwk", { kty: "RSA", n, e, alg: "RS256", ext: true }, { name: "RSASSA-PKCS1-v1_5", hash: "SHA-256" }, false, ["verify"]);
  assert.equal(await crypto.subtle.verify("RSASSA-PKCS1-v1_5", key, bytesOfHex(signature), image), true);
  assert.equal(await crypto.subtle.verify("RSASSA-PKCS1-v1_5", key, bytesOfHex(signature), new TextEncoder().encode("another image")), false);
});

test("every run signs with a key of its own", async () => {
  const image = new Uint8Array(8);
  const a = await signImage(image);
  const b = await signImage(image);
  assert.notDeepEqual([...a.pubkey], [...b.pubkey]);
});

test("a key that is not RSA-2048 is refused", () => {
  const short = Buffer.alloc(128, 1).toString("base64url");
  assert.throws(() => publicKeyBytes({ n: short, e: "AQAB" }), /RSA-2048/);
});
