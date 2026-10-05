create table if not exists public.veetype_subscriptions (
    user_id uuid primary key references auth.users (id) on delete cascade,
    subscription_id text not null unique,
    store_id text not null,
    variant_id text not null,
    status text not null,
    current_period_ends_at timestamptz,
    last_event_at timestamptz not null,
    updated_at timestamptz not null default now()
);

alter table public.veetype_subscriptions enable row level security;

create table if not exists public.veetype_webhook_events (
    event_digest text primary key,
    processed_at timestamptz not null default now()
);

alter table public.veetype_webhook_events enable row level security;

create or replace function public.apply_veetype_subscription_event(
    p_event_digest text,
    p_user_id uuid,
    p_subscription_id text,
    p_store_id text,
    p_variant_id text,
    p_status text,
    p_period_end timestamptz,
    p_event_at timestamptz
) returns void
language plpgsql
security definer
set search_path = ''
as $$
begin
    insert into public.veetype_webhook_events (event_digest)
    values (p_event_digest)
    on conflict do nothing;

    if not found then
        return;
    end if;

    insert into public.veetype_subscriptions (
        user_id,
        subscription_id,
        store_id,
        variant_id,
        status,
        current_period_ends_at,
        last_event_at,
        updated_at
    )
    values (
        p_user_id,
        p_subscription_id,
        p_store_id,
        p_variant_id,
        p_status,
        p_period_end,
        p_event_at,
        now()
    )
    on conflict (user_id) do update set
        subscription_id = excluded.subscription_id,
        store_id = excluded.store_id,
        variant_id = excluded.variant_id,
        status = excluded.status,
        current_period_ends_at = excluded.current_period_ends_at,
        last_event_at = excluded.last_event_at,
        updated_at = now()
    where public.veetype_subscriptions.last_event_at <= excluded.last_event_at;
end;
$$;

revoke all on function public.apply_veetype_subscription_event(
    text, uuid, text, text, text, text, timestamptz, timestamptz
) from public, anon, authenticated;
grant execute on function public.apply_veetype_subscription_event(
    text, uuid, text, text, text, text, timestamptz, timestamptz
) to service_role;

revoke all on public.veetype_subscriptions from anon, authenticated;
revoke all on public.veetype_webhook_events from anon, authenticated;
