-- SPDX-FileCopyrightText: Copyright Logically Magic Inc. <hello@logicallymagic.com>
-- SPDX-FileCopyrightText: Copyright Hamid Jahanzad <Hamid@Jahanzad.com>
-- SPDX-FileCopyrightText: Copyright Contributors to the Exchange8:SOR
-- SPDX-License-Identifier: Apache-2.0

-- Ex8:SOR — routing tokens/options/connections, routed orders, venue charges.
--
-- Design: design/sentin8-exchange8-data-model-technical-design.md §3.3 (SOR).
-- No migration code (new platform): `sentin8-cli vertical init` applies this.

CREATE SCHEMA IF NOT EXISTS sor;

-- Token(1) → mic / ex_destination / fix_block (legacy ROUTING_TOKEN).
CREATE TABLE IF NOT EXISTS sor.routing_token (
    id              UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    env_id          UUID NOT NULL,
    token           CHAR(1) NOT NULL,
    mic             TEXT NOT NULL,
    ex_destination  TEXT,
    fix_block       TEXT,                -- raw extra FIX tags appended verbatim
    UNIQUE (env_id, token)
);

-- Marketable option codes (legacy ROUTING_OPTION): type Dark/Lit/Post.
CREATE TABLE IF NOT EXISTS sor.routing_option (
    id          UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    env_id      UUID NOT NULL,
    option_code INTEGER NOT NULL,
    option_type TEXT NOT NULL CHECK (option_type IN ('Dark','Lit','Post')),
    tokens      TEXT[] NOT NULL DEFAULT '{}',
    price_factor NUMERIC(10,4),
    dynamic_seq BOOLEAN NOT NULL DEFAULT FALSE,
    status      TEXT NOT NULL DEFAULT 'E' CHECK (status IN ('E','D')),
    UNIQUE (env_id, option_code, option_type)
);

-- Outbound venue FIX sessions (legacy ROUTING_CONNECTION).
CREATE TABLE IF NOT EXISTS sor.routing_connection (
    id              UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    env_id          UUID NOT NULL,
    mic             TEXT NOT NULL,
    exec_broker_id  TEXT NOT NULL,
    slice_id        TEXT,                -- market-slice key (A–Z)
    sender_comp_id  TEXT NOT NULL,
    target_comp_id  TEXT NOT NULL,
    address         TEXT NOT NULL,
    port            INTEGER NOT NULL,
    heartbeat_int   INTEGER NOT NULL DEFAULT 30,
    creds_secret_ref TEXT,               -- Sentin8:Secrets reference — never plaintext
    hidden          BOOLEAN NOT NULL DEFAULT FALSE,
    enabled         BOOLEAN NOT NULL DEFAULT TRUE
);

-- Routed orders and legs.
CREATE TABLE IF NOT EXISTS sor.routed_order (
    id              UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    env_id          UUID NOT NULL,
    order_id        BIGINT NOT NULL,
    strategy_code   TEXT NOT NULL,       -- 9 digits [DD][C][LLLL][PP]
    state           TEXT NOT NULL DEFAULT 'pending'
                    CHECK (state IN ('pending','routing','posted','complete','killed')),
    UNIQUE (env_id, order_id)
);
CREATE TABLE IF NOT EXISTS sor.routed_leg (
    id              UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    env_id          UUID NOT NULL,
    routed_order_id UUID NOT NULL REFERENCES sor.routed_order(id),
    leg_seq         INTEGER NOT NULL,
    venue_cl_ord_id TEXT NOT NULL,       -- <platform_order_id>-<leg_seq>
    mic             TEXT NOT NULL,
    phase           TEXT NOT NULL CHECK (phase IN
                    ('radar','spray','sequential','post')),
    state           TEXT NOT NULL DEFAULT 'pending'
                    CHECK (state IN ('pending','accepted','filled','rejected','killed')),
    UNIQUE (routed_order_id, leg_seq)
);

-- Venue fills + amendments.
CREATE TABLE IF NOT EXISTS sor.routing_trade (
    id          UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    env_id      UUID NOT NULL,
    routed_leg_id UUID NOT NULL REFERENCES sor.routed_leg(id),
    venue_trade_id TEXT,
    qty         NUMERIC(18,8) NOT NULL,
    price       NUMERIC(18,8) NOT NULL,
    liquidity   CHAR(1) CHECK (liquidity IN ('A','R')),
    ts          TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE TABLE IF NOT EXISTS sor.routing_trade_amend (
    id              UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    env_id          UUID NOT NULL,
    routing_trade_id UUID NOT NULL REFERENCES sor.routing_trade(id),
    new_qty         NUMERIC(18,8),
    new_price       NUMERIC(18,8),
    amended_by      TEXT NOT NULL,
    amended_at      TIMESTAMPTZ NOT NULL DEFAULT now()
);

-- Venue charges (mic × market × price band → charge).
CREATE TABLE IF NOT EXISTS sor.trade_venue_charge (
    id          UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    env_id      UUID NOT NULL,
    mic         TEXT NOT NULL,
    market_code TEXT NOT NULL,
    price_low   NUMERIC(18,4) NOT NULL,
    price_high  NUMERIC(18,4),
    charge      NUMERIC(18,8) NOT NULL,
    active      BOOLEAN NOT NULL DEFAULT TRUE
);

-- Symbol-letter venue slices (legacy MARKET_SLICE).
CREATE TABLE IF NOT EXISTS sor.market_slice (
    id          UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    env_id      UUID NOT NULL,
    slice_id    CHAR(1) NOT NULL,        -- A–Z
    mic         TEXT NOT NULL,
    symbol_begin TEXT NOT NULL,
    symbol_end  TEXT NOT NULL,
    UNIQUE (env_id, slice_id, mic)
);

-- ---------------------------------------------------------------------------
-- RLS (architecture-principles.md §4.3): enable + force on all tenant tables.
-- Policies use the platform helpers sentin8_context_initialized() and
-- sentin8_current_env_id(); services call set_tenant_context on the same
-- connection as the data query.
-- ---------------------------------------------------------------------------
DO $$
DECLARE tbl TEXT;
BEGIN
    FOREACH tbl IN ARRAY ARRAY['sor.routing_token', 'sor.routing_option', 'sor.routing_connection', 'sor.routed_order', 'sor.routed_leg', 'sor.routing_trade', 'sor.routing_trade_amend', 'sor.trade_venue_charge', 'sor.market_slice'] LOOP
        BEGIN
            EXECUTE format('ALTER TABLE %s ENABLE ROW LEVEL SECURITY', tbl);
            EXECUTE format('ALTER TABLE %s FORCE ROW LEVEL SECURITY', tbl);
            EXECUTE format('DROP POLICY IF EXISTS %1$I_select ON %2$s;
CREATE POLICY %1$I_select ON %2$s FOR SELECT USING (sentin8_context_initialized() AND env_id = sentin8_current_env_id())', split_part(tbl, '.', 2), tbl);
            EXECUTE format('DROP POLICY IF EXISTS %1$I_insert ON %2$s;
CREATE POLICY %1$I_insert ON %2$s FOR INSERT WITH CHECK (sentin8_context_initialized() AND env_id = sentin8_current_env_id())', split_part(tbl, '.', 2), tbl);
            EXECUTE format('DROP POLICY IF EXISTS %1$I_update ON %2$s;
CREATE POLICY %1$I_update ON %2$s FOR UPDATE USING (sentin8_context_initialized() AND env_id = sentin8_current_env_id())', split_part(tbl, '.', 2), tbl);
            EXECUTE format('DROP POLICY IF EXISTS %1$I_delete ON %2$s;
CREATE POLICY %1$I_delete ON %2$s FOR DELETE USING (sentin8_context_initialized() AND env_id = sentin8_current_env_id())', split_part(tbl, '.', 2), tbl);
        EXCEPTION WHEN OTHERS THEN
            RAISE NOTICE 'RLS setup failed for %: %', tbl, SQLERRM;
        END;
    END LOOP;
END $$;

-- ---------------------------------------------------------------------------
-- Per-service role grants (schema-per-service: this service owns schema
-- `sor`; grants live in db `exchange8` only — design doc §1).
-- ---------------------------------------------------------------------------
DO $$
BEGIN
    IF EXISTS (SELECT 1 FROM pg_roles WHERE rolname = 'exchange8_sor_service') THEN
        ALTER ROLE exchange8_sor_service SET search_path = sor, public;
        GRANT USAGE ON SCHEMA sor TO exchange8_sor_service;
        GRANT SELECT, INSERT, UPDATE, DELETE ON ALL TABLES IN SCHEMA sor TO exchange8_sor_service;
        GRANT USAGE, SELECT ON ALL SEQUENCES IN SCHEMA sor TO exchange8_sor_service;
    END IF;
END $$;

-- ---------------------------------------------------------------------------
-- Local outbox (DBX §6.7 — generated from `sentin8_outbox::outbox_table_sql(
-- "sor")`): rows written in the same DB transaction as the domain change;
-- the shell's relay (boot step §8b) pushes them to Bus `POST /v1/produce`.
-- ---------------------------------------------------------------------------

CREATE TABLE IF NOT EXISTS sor.outbox (
    id              UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    env_id          UUID NOT NULL,
    event_type      TEXT NOT NULL,
    stream_family   TEXT NOT NULL,
    payload         JSONB NOT NULL,
    publisher_id_token  TEXT,
    publisher_subject   TEXT,
    relayed         BOOLEAN NOT NULL DEFAULT false,
    relayed_at      TIMESTAMPTZ,
    relay_attempts  INT NOT NULL DEFAULT 0,
    next_retry_at   TIMESTAMPTZ,
    created_at      TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX IF NOT EXISTS idx_sor_outbox_unrelayed
    ON sor.outbox (created_at) WHERE relayed = false;
DO $$ BEGIN
    EXECUTE 'ALTER TABLE sor.outbox ENABLE ROW LEVEL SECURITY';
    EXECUTE 'ALTER TABLE sor.outbox FORCE ROW LEVEL SECURITY';
    EXECUTE 'DROP POLICY IF EXISTS outbox_select ON sor.outbox';
    EXECUTE 'CREATE POLICY outbox_select ON sor.outbox FOR SELECT
        USING (sentin8_context_initialized()
               AND env_id = sentin8_current_env_id())';
    EXECUTE 'DROP POLICY IF EXISTS outbox_insert ON sor.outbox';
    EXECUTE 'CREATE POLICY outbox_insert ON sor.outbox FOR INSERT
        WITH CHECK (sentin8_context_initialized()
               AND env_id = sentin8_current_env_id())';
    EXECUTE 'DROP POLICY IF EXISTS outbox_update ON sor.outbox';
    EXECUTE 'CREATE POLICY outbox_update ON sor.outbox FOR UPDATE
        USING (sentin8_context_initialized()
               AND env_id = sentin8_current_env_id())';
EXCEPTION WHEN OTHERS THEN
    RAISE NOTICE 'RLS setup failed for sor.outbox: %', SQLERRM;
END $$;

CREATE OR REPLACE FUNCTION sor.sor_notify_outbox() RETURNS TRIGGER
LANGUAGE plpgsql AS $fn$
BEGIN
    PERFORM pg_notify('sor_outbox_changed', '');
    RETURN NEW;
END;
$fn$;
DROP TRIGGER IF EXISTS outbox_notify_trigger ON sor.outbox;
CREATE TRIGGER outbox_notify_trigger
    AFTER INSERT ON sor.outbox
    FOR EACH ROW
    EXECUTE FUNCTION sor.sor_notify_outbox();

-- ---------------------------------------------------------------------------
-- Local audit journal (DBX §6.8 — generated from
-- `sentin8_audit::audit_journal_sql("sor")`): rows written in the same
-- transaction as the domain change; the shell's pusher (boot step §8c) pushes
-- them to Sys `POST /v1/audit` (`sentin8:sys:audit:write`). A Sys outage
-- never drops audit.
-- ---------------------------------------------------------------------------

CREATE TABLE IF NOT EXISTS sor.audit_journal (
    id              UUID PRIMARY KEY,
    env_id          UUID NOT NULL,
    action          TEXT NOT NULL,
    target_resource_type TEXT,
    target_resource_id    TEXT,
    actor_user_id   TEXT,
    actor_subject_type TEXT,
    severity        TEXT NOT NULL DEFAULT 'info',
    correlation_id  TEXT,
    before_state    JSONB,
    after_state     JSONB,
    details         JSONB,
    pushed          BOOLEAN NOT NULL DEFAULT false,
    pushed_at       TIMESTAMPTZ,
    created_at      TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX IF NOT EXISTS idx_sor_audit_journal_unpushed
    ON sor.audit_journal (created_at) WHERE pushed = false;
DO $$ BEGIN
    EXECUTE 'ALTER TABLE sor.audit_journal ENABLE ROW LEVEL SECURITY';
    EXECUTE 'ALTER TABLE sor.audit_journal FORCE ROW LEVEL SECURITY';
    EXECUTE 'DROP POLICY IF EXISTS audit_journal_dml ON sor.audit_journal';
    EXECUTE 'CREATE POLICY audit_journal_dml ON sor.audit_journal
        USING (sentin8_context_initialized()
               AND env_id = sentin8_current_env_id())
        WITH CHECK (sentin8_context_initialized()
               AND env_id = sentin8_current_env_id())';
EXCEPTION WHEN OTHERS THEN
    RAISE NOTICE 'RLS setup failed for sor.audit_journal: %', SQLERRM;
END $$;

-- Outbox + journal role grants (same pattern as the table grants above).
DO $$
BEGIN
    IF EXISTS (SELECT 1 FROM pg_roles WHERE rolname = 'exchange8_sor_service') THEN
        GRANT SELECT, INSERT, UPDATE ON ALL TABLES IN SCHEMA sor TO exchange8_sor_service;
    END IF;
END $$;
