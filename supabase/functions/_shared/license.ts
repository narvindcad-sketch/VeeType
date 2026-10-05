const LICENSE_ISSUER = "veetype-license";
const LICENSE_AUDIENCE = "veetype-desktop";
const LICENSE_DURATION_SECONDS = 7 * 24 * 60 * 60;

function decodeBase64(value: string): Uint8Array {
  const binary = atob(value);
  return Uint8Array.from(binary, (character) => character.charCodeAt(0));
}

function encodeBase64Url(value: Uint8Array): string {
  let binary = "";
  for (const byte of value) binary += String.fromCharCode(byte);
  return btoa(binary).replaceAll("+", "-").replaceAll("/", "_").replace(/=+$/, "");
}

export async function issueLicense(
  userId: string,
  periodEnd: number | null,
): Promise<{ license: string; expiresAt: number }> {
  const now = Math.floor(Date.now() / 1000);
  const expiresAt = Math.min(
    now + LICENSE_DURATION_SECONDS,
    periodEnd ?? now + LICENSE_DURATION_SECONDS,
  );
  if (expiresAt <= now) throw new Error("The subscription period has expired");

  const privateKeyBase64 = Deno.env.get("VEETYPE_LICENSE_PRIVATE_KEY_PKCS8_B64");
  if (!privateKeyBase64) throw new Error("License signing key is not configured");
  const privateKey = await crypto.subtle.importKey(
    "pkcs8",
    decodeBase64(privateKeyBase64),
    { name: "Ed25519" },
    false,
    ["sign"],
  );
  const header = encodeBase64Url(
    new TextEncoder().encode(JSON.stringify({ alg: "EdDSA", typ: "JWT" })),
  );
  const claims = encodeBase64Url(
    new TextEncoder().encode(
      JSON.stringify({
        iss: LICENSE_ISSUER,
        aud: LICENSE_AUDIENCE,
        sub: userId,
        iat: now,
        exp: expiresAt,
        entitlements: ["cloud_providers", "hands_free", "large_models"],
      }),
    ),
  );
  const content = `${header}.${claims}`;
  const signature = new Uint8Array(
    await crypto.subtle.sign("Ed25519", privateKey, new TextEncoder().encode(content)),
  );
  return { license: `${content}.${encodeBase64Url(signature)}`, expiresAt };
}
