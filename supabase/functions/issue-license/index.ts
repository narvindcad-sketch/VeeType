import { createClient } from "npm:@supabase/supabase-js@2";
import { issueLicense } from "../_shared/license.ts";

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
    const authorization = request.headers.get("Authorization");
    const accessToken = authorization?.match(/^Bearer\s+(.+)$/i)?.[1];
    if (!accessToken) {
      return Response.json({ error: "Authentication required" }, { status: 401, headers: corsHeaders });
    }

    const supabaseUrl = Deno.env.get("SUPABASE_URL");
    const anonKey = Deno.env.get("SUPABASE_ANON_KEY");
    const serviceKey = Deno.env.get("SUPABASE_SERVICE_ROLE_KEY");
    if (!supabaseUrl || !anonKey || !serviceKey) {
      throw new Error("Supabase service configuration is incomplete");
    }
    const authClient = createClient(supabaseUrl, anonKey, {
      auth: { persistSession: false },
    });
    const { data: { user }, error: authError } = await authClient.auth.getUser(accessToken);
    if (authError || !user) {
      return Response.json({ error: "Invalid or expired login session" }, { status: 401, headers: corsHeaders });
    }

    const admin = createClient(supabaseUrl, serviceKey, {
      auth: { persistSession: false, autoRefreshToken: false },
    });
    const { data: subscription, error } = await admin
      .from("veetype_subscriptions")
      .select("status,current_period_ends_at")
      .eq("user_id", user.id)
      .maybeSingle();
    if (error) throw error;

    const now = Date.now();
    const periodEnd = subscription?.current_period_ends_at
      ? Date.parse(subscription.current_period_ends_at)
      : null;
    const entitledStatus = subscription?.status === "active" || subscription?.status === "on_trial";
    if (!entitledStatus || (periodEnd !== null && (!Number.isFinite(periodEnd) || periodEnd <= now))) {
      return Response.json({ error: "No active VeeType Pro subscription was found" }, { status: 403, headers: corsHeaders });
    }

    const license = await issueLicense(user.id, periodEnd === null ? null : Math.floor(periodEnd / 1000));
    return Response.json(license, { headers: corsHeaders });
  } catch (error) {
    console.error("License issuance failed", error);
    return Response.json(
      { error: "Could not issue a license" },
      { status: 500, headers: corsHeaders },
    );
  }
});
