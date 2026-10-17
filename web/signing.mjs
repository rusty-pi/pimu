// Signs a boot.img the way HTTP boot wants it signed.
//
// The bootloader only boots a signed image over HTTP. `boot.sig` is what
// `rpi-eeprom-digest -k` writes: the image's SHA-256, a timestamp, and an
// RSA-2048 PKCS#1 v1.5 signature over the image's SHA-256, as hex. The bootloader
// checks it against the public key in the EEPROM's `pubkey.bin`: the modulus and
// then the public exponent, little-endian, 256 + 8 bytes. A key is made for each
// run, so what signs the image is something nobody else has.

const hex = (bytes) => [...bytes].map((b) => b.toString(16).padStart(2, "0")).join("");

function base64url(text) {
  const padded = text.replace(/-/g, "+").replace(/_/g, "/").padEnd(Math.ceil(text.length / 4) * 4, "=");
  return Uint8Array.from(atob(padded), (c) => c.charCodeAt(0));
}

// The public half of a JWK RSA key as `pubkey.bin` holds it.
export function publicKeyBytes(jwk) {
  const modulus = base64url(jwk.n).reverse();
  const exponent = base64url(jwk.e).reverse();
  if (modulus.length !== 256) throw new Error(`a ${modulus.length * 8}-bit key: the bootloader takes RSA-2048`);
  const out = new Uint8Array(264);
  out.set(modulus, 0);
  out.set(exponent, 256);
  return out;
}

// `{ sig, pubkey }` for `image`: the text of boot.sig and the 264 bytes for the EEPROM.
export async function signImage(image, now = Date.now()) {
  const subtle = globalThis.crypto.subtle;
  const pair = await subtle.generateKey(
    { name: "RSASSA-PKCS1-v1_5", modulusLength: 2048, publicExponent: new Uint8Array([1, 0, 1]), hash: "SHA-256" },
    true,
    ["sign", "verify"],
  );
  const signature = new Uint8Array(await subtle.sign("RSASSA-PKCS1-v1_5", pair.privateKey, image));
  const digest = new Uint8Array(await subtle.digest("SHA-256", image));
  const text = `${hex(digest)}\nts: ${Math.floor(now / 1000)}\nrsa2048: ${hex(signature)}\n`;
  return { sig: new TextEncoder().encode(text), pubkey: publicKeyBytes(await subtle.exportKey("jwk", pair.publicKey)) };
}
