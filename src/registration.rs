// SPDX-FileCopyrightText: Copyright Logically Magic Inc. <hello@logicallymagic.com>
// SPDX-FileCopyrightText: Copyright Hamid Jahanzad <Hamid@Jahanzad.com>
// SPDX-FileCopyrightText: Copyright Contributors to the Exchange8:SOR
// SPDX-License-Identifier: Apache-2.0

//! Service self-registration data for Exchange8:SOR (generated; hand-tune freely).
//!
//! Output via `--print-registration` (see the platform
//! `sentin8-service-self-registration-design.md`).

use sentin8::registration::{
    AdminScope, AdminUi, AdminUiPage, ErrorCode, M2mGrant, PgRole, PolicySetDecl, Registration,
    RelationDecl, SqlMigration,
};

/// The Shield text of the declared scope-mirror set — public so the
/// policy wiring tests (see DESIGN.md §9) assert against the REAL seed.
pub const SCOPE_MIRROR_POLICIES: &str = "permit(principal, action == Action::\"exchange8:sor:decide\", resource) when { principal.scopes.contains(\"exchange8:sor:decide\") };
permit(principal, action == Action::\"exchange8:sor:decide\", resource) when { principal.scopes.contains(\"sentin8:sys:admin\") };
permit(principal, action == Action::\"exchange8:sor:decide\", resource) when { principal.scopes.contains(\"sentin8:sys:org:admin\") };
permit(principal, action == Action::\"exchange8:sor:decide\", resource) when { principal.scopes.contains(\"sentin8:sys:env:admin\") };";

/// Default declarative policy set (seeded by `vertical enroll` as
/// `origin='platform'`, `mode='enforced'`): action-granular scope permits
/// over the LIVE effective scope set — the exact mirror of the gates this
/// service's handlers evaluate in step 5 (see DESIGN.md §5). Enforced:
/// step 5 is the only gate; shadow is the emergency one-UPDATE rollback
/// lever only.
const POLICIES: &[PolicySetDecl] = &[PolicySetDecl {
    policy_set_name: "scope-mirror",
    description: "Action-granular scope permits for exchange8:sor over the LIVE effective scope set. Enforced — step 5 is the only gate.",
    // Shield text lives in SCOPE_MIRROR_POLICIES (single source).
    policies: SCOPE_MIRROR_POLICIES,
    schema: None,
    mode: "enforced",
    default_enabled: true,
}];

/// Default ReBAC relation tuples (design §3.1c) — none declared by this
/// service today; env admins author their own graph via the relations
/// admin API.
const RELATIONS: &[RelationDecl] = &[];

const PG_ROLES: &[PgRole] = &[PgRole {
    scope: "exchange8:routing:read,exchange8:routing:write",
    role: "exchange8_sor_service",
    bypass_rls: false,
    // Grants live in the exchange8 vertical database only
    // (database-per-vertical, schema-per-service — see DESIGN.md §1/§3).
    database: "exchange8",
    // This service owns all its reads — no cross-schema grants.
    cross_schema_grants: &[],
}];

const ADMIN_SCOPES: &[AdminScope] = &[
    AdminScope::Typed {
        role: "exchange8:admin",
        scope: "exchange8:routing:read",
    },
    AdminScope::Typed {
        role: "exchange8:admin",
        scope: "exchange8:routing:write",
    },
    // Policy-admin surface (per-service policy scope — see DESIGN.md §4).
    AdminScope::Typed {
        role: "exchange8:sor:admin",
        scope: "exchange8:sor:policy:read",
    },
    AdminScope::Typed {
        role: "exchange8:sor:admin",
        scope: "exchange8:sor:policy:write",
    },
];

const ERROR_CODES: &[ErrorCode] = &[ErrorCode {
    enum_name: "Exchange8StrategyCodeInvalid",
    error_code: "exchange8_strategy_code_invalid",
    status_code: 422,
}];

const ADMIN_UI_PAGES: &[AdminUiPage] = &[
    AdminUiPage {
        route: "routing",
        label: "Routing",
        ui_endpoint: "/v1/admin/ui/routing",
        data_endpoint: "/v1/admin/routing",
        icon: "0xe8d2",
    },
    AdminUiPage {
        route: "legs",
        label: "Routed Legs",
        ui_endpoint: "/v1/admin/ui/legs",
        data_endpoint: "/v1/admin/legs",
        icon: "0xe5d9",
    },
];

/// Workload scope grants (platform self-registration; one entry per
/// workload this service presents a certificate for; `vertical enroll` upserts the
/// rows so the fail-closed registry map admits the registration. Scopes use the
/// full vocabulary form:
/// - `exchange8:sor` / `sor` — the service's own client (both
///   spellings granted; runtime call sites use the bare short name, the
///   map looks up the exact workload): bus produce/consume + the
///   shared certificate identity.
/// - `exchange8:sor-policy` / `sor-policy` — Shield hot-reload
///   consumer + stat emitter pair.
/// - `sor-audit` — shell audit-journal pusher workload (see DESIGN.md §5).
/// - `exchange8-sor-tls` — database-free HostContextResolver resolve workload
///   (TLS-RLS-4: `GET /v1/resolve/hostname`, scope
///   `sentin8:sys:instances:read`).
const M2M_GRANTS: &[M2mGrant] = &[
    M2mGrant {
        workload: "exchange8:sor",
        scopes: &[
            "exchange8:routing:read",
            "exchange8:routing:write",
            "sentin8:bus:produce",
            "sentin8:bus:consume",
            "sentin8:stat:measure",
        ],
    },
    M2mGrant {
        workload: "sor",
        scopes: &[
            "exchange8:routing:read",
            "exchange8:routing:write",
            "sentin8:bus:produce",
            "sentin8:bus:consume",
            "sentin8:stat:measure",
        ],
    },
    M2mGrant {
        workload: "exchange8:sor-policy",
        scopes: &["sentin8:bus:consume", "sentin8:stat:measure"],
    },
    M2mGrant {
        workload: "sor-policy",
        scopes: &["sentin8:stat:measure"],
    },
    M2mGrant {
        workload: "sor-audit",
        scopes: &["sentin8:sys:audit:write"],
    },
    M2mGrant {
        workload: "exchange8-sor-tls",
        scopes: &["sentin8:sys:instances:read"],
    },
];

const SQL_1_INIT: &str = include_str!("../sql/init.sql");

fn empty_openapi() -> serde_json::Value {
    serde_json::Value::Null
}

pub fn print(section: Option<String>) -> anyhow::Result<()> {
    let reg = Registration {
        service_type: "exchange8:sor",
        subdomain: "exchange8-sor",
        health_path: "/v1/health",
        admin_scopes: ADMIN_SCOPES,
        error_codes: ERROR_CODES,
        consent_providers: &[],
        consent_flows: &[],
        admin_ui: Some(AdminUi {
            label: "SOR",
            icon: "0xe5d3",
            service_flag: "exchange8-sor",
            pages: ADMIN_UI_PAGES,
        }),
        sql_migrations: vec![SqlMigration {
            version: 1,
            name: "init",
            sql: SQL_1_INIT,
        }],
        pg_roles: PG_ROLES,
        audit_findings: None,
        service_actions: None,
        m2m_grants: Some(M2M_GRANTS),
        openapi_url: "/openapi.json",
        openapi: empty_openapi,
        policies: POLICIES,
        relations: RELATIONS,
        event_triggers: &[],
        schedules: &[],
    };
    sentin8::registration::print_registration(section, &reg, empty_openapi)
}
