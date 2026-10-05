import { createClient } from "npm:@supabase/supabase-js@2";

const corsHeaders = {
  "Access-Control-Allow-Origin": "*",
  "Access-Control-Allow-Headers": "authorization, apikey, content-type",
};

Deno.serve(async (request) => {
  if (request.method === "OPTIONS") {
    return new Response("ok", { headers: corsHeaders });
  }
  if (request.method !== "POST") {
    return Response.json({ error: "Method not allowed" }, { status: 405, headers: corsHeaders });
  }

  try {
    const accessToken = request.headers.get("Authorization")?.match(/^Bearer\s+(.+)$/i)?.[1];
    if (!accessToken) {
      return Response.json({ error: "Authentication required" }, { status: 401, headers: corsHeaders });
    }
    const supabaseUrl = Deno.env.get("SUPABASE_URL");
    const anonKey = Deno.env.get("SUPABASE_ANON_KEY");
    if (!supabaseUrl || !anonKey) throw new Error("Supabase Auth is not configured");
    const authClient = createClient(supabaseUrl, anonKey, {
      auth: { persistSession: false },
    });
    const { data: { user }, error: authError } = await authClient.auth.getUser(accessToken);
    if (authError || !user) {
      return Response.json({ error: "Invalid or expired login session" }, { status: 401, headers: corsHeaders });
    }

    const apiKey = Deno.env.get("LEMON_SQUEEZY_API_KEY");
    const storeId = Deno.env.get("LEMON_SQUEEZY_STORE_ID");
    const variantId = Deno.env.get("LEMON_SQUEEZY_PRO_VARIANT_ID");
    if (!apiKey || !storeId || !variantId) {
      throw new Error("Lemon Squeezy checkout configuration is incomplete");
    }
    const response = await fetch("https://api.lemonsqueezy.com/v1/checkouts", {
      method: "POST",
      headers: {
        "Accept": "application/vnd.api+json",
        "Content-Type": "application/vnd.api+json",
        "Authorization": `Bearer ${apiKey}`,
      },
      body: JSON.stringify({
        data: {
          type: "checkouts",
          attributes: {
            checkout_data: {
              email: user.email,
              custom: { supabase_user_id: user.id },
            },
          },
          relationships: {
            store: { data: { type: "stores", id: storeId } },
            variant: { data: { type: "variants", id: variantId } },
          },
        },
      }),
    });
    const result = await response.json();
    if (!response.ok) {
      console.error("Lemon Squeezy checkout error", response.status, result);
      return Response.json({ error: "Checkout provider rejected the request" }, { status: 502, headers: corsHeaders });
    }
    const checkoutUrl = result?.data?.attributes?.url;
    if (typeof checkoutUrl !== "string" || !checkoutUrl.startsWith("https://")) {
      throw new Error("Lemon Squeezy returned an invalid checkout URL");
    }
    return Response.json({ checkout_url: checkoutUrl }, { headers: corsHeaders });
  } catch (error) {
    console.error("Checkout creation failed", error);
    return Response.json(
      { error: "Could not create a checkout session" },
      { status: 500, headers: corsHeaders },
    );
  }
});
