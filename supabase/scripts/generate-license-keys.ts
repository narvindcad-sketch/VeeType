const pair = await crypto.subtle.generateKey({ name: "Ed25519" }, true, ["sign", "verify"]);
const privateKey = new Uint8Array(await crypto.subtle.exportKey("pkcs8", pair.privateKey));
const publicKey = new Uint8Array(await crypto.subtle.exportKey("raw", pair.publicKey));

const base64 = (bytes: Uint8Array) => {
  let binary = "";
  for (const byte of bytes) binary += String.fromCharCode(byte);
  return btoa(binary);
};

console.log("Store only in Supabase Edge Function secrets:");
console.log(`VEETYPE_LICENSE_PRIVATE_KEY_PKCS8_B64=${base64(privateKey)}`);
console.log("");
console.log("Set at build time for the desktop app (public key only):");
console.log(`VEETYPE_LICENSE_PUBLIC_KEY_B64=${base64(publicKey)}`);
