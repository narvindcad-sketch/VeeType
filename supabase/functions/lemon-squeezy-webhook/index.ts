import { createClient } from "npm:@supabase/supabase-js@2";

function constantTimeEqual(left: string, right: string): boolean {
  if (left.length !== right.length) return false;
  let difference = 0;
  for (let index = 0; index < left.length; index++) {
    difference |= left.charCodeAt(index) ^ right.charCodeAt(index);
  }
  return difference === 0;
}

async function validSignature(secret: string, body: string, received: string): Promise<boolean> {
  const key = await crypto.subtle.importKey(
    "raw",
    new TextEncoder().encode(secret),
    { name: "HMAC", hash: "SHA-256" },
    false,
    ["sign"],
  );
  const digest = new Uint8Array(
    await crypto.subtle.sign("HMAC", key, new TextEncoder().encode(body)),
  );
  const expected = Array.from(digest, (byte) => byte.toString(16).padStart(2, "0")).join("");
  return constantTimeEqual(expected, received.toLowerCase());
}

Deno.serve(async (request) => {
  if (request.method !== "POST") {
    return Response.json({ error: "Method not allowed" }, { status: 405 });
  }
  try {
    const secret = Deno.env.get("LEMON_SQUEEZY_WEBHOOK_SECRET");
    if (!secret) throw new Error("Webhook signing secret is not configured");
    const body = await request.text();
    const signature = request.headers.get("X-Signature") ?? "";
    if (!(await validSignature(secret, body, signature))) {
      return Response.json({ error: "Invalid webhook signature" }, { status: 401 });
    }

    const event = JSON.parse(body);
    const eventName = event?.meta?.event_name;
    const attributes = event?.data?.attributes;
    const storeId = String(attributes?.store_id ?? "");
    const variantId = String(attributes?.variant_id ?? "");
    const expectedStoreId = Deno.env.get("LEMON_SQUEEZY_STORE_ID");
    const expectedVariantId = Deno.env.get("LEMON_SQUEEZY_PRO_VARIANT_ID");
    if (storeId !== expectedStoreId || variantId !== expectedVariantId) {
      return Response.json({ error: "Unexpected store or product" }, { status: 400 });
    }
    const userId = event?.meta?.custom_data?.supabase_user_id;
    if (typeof userId !== "string" || !/^[0-9a-f-]{36}$/i.test(userId)) {
      return Response.json({ error: "Checkout is missing its account association" }, { status: 400 });
    }

    const allowedEvents = new Set([
      "subscription_created",
      "subscription_updated",
      "subscription_cancelled",
      "subscription_expired",
      "subscription_paused",
      "subscription_resumed",
      "subscription_unpaused",
      "subscription_payment_success",
      "subscription_payment_failed",
      "subscription_payment_recovered",
    ]);
    if (!allowedEvents.has(eventName)) return Response.json({ received: true });

    const forcedStatuses: Record<string, string> = {
      subscription_cancelled: "cancelled",
      subscription_expired: "expired",
      subscription_paused: "paused",
      subscription_payment_failed: "past_due",
      subscription_payment_success: "active",
      subscription_payment_recovered: "active",
      subscription_resumed: "active",
      subscription_unpaused: "active",
    };
    const status = forcedStatuses[eventName] ?? attributes?.status;
    if (typeof status !== "string") {
      return Response.json({ error: "Subscription event has no status" }, { status: 400 });
    }
    const subscriptionId =
      String(attributes?.subscription_id ?? event?.data?.id ?? "");
    if (!subscriptionId) {
      return Response.json({ error: "Subscription event has no subscription ID" }, { status: 400 });
    }
    const periodEnd = attributes?.renews_at ?? attributes?.ends_at ?? null;
    const periodEndIso = typeof periodEnd === "string" ? periodEnd : null;
    if (periodEndIso && !Number.isFinite(Date.parse(periodEndIso))) {
      return Response.json({ error: "Subscription period end is invalid" }, { status: 400 });
    }

    const supabaseUrl = Deno.env.get("SUPABASE_URL");
    const serviceKey = Deno.env.get("SUPABASE_SERVICE_ROLE_KEY");
    if (!supabaseUrl || !serviceKey) throw new Error("Supabase admin configuration is incomplete");
    const admin = createClient(supabaseUrl, serviceKey, {
      auth: { persistSession: false, autoRefreshToken: false },
    });
    const eventDigest = await crypto.subtle.digest("SHA-256", new TextEncoder().encode(body));
    const digestHex = Array.from(new Uint8Array(eventDigest), (byte) =>
      byte.toString(16).padStart(2, "0")
    ).join("");
    const { error } = await admin.rpc("apply_veetype_subscription_event", {
      p_event_digest: digestHex,
      p_user_id: userId,
      p_subscription_id: subscriptionId,
      p_store_id: storeId,
      p_variant_id: variantId,
      p_status: status,
      p_period_end: periodEndIso,
      p_event_at: attributes?.updated_at ?? new Date().toISOString(),
    });
    if (error) throw error;
    return Response.json({ received: true });
  } catch (error) {
    console.error("Lemon Squeezy webhook failed", error);
    return Response.json({ error: "Could not process subscription event" }, { status: 500 });
  }
});
