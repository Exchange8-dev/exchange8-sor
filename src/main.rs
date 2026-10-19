// SPDX-FileCopyrightText: Copyright Logically Magic Inc. <hello@logicallymagic.com>
// SPDX-FileCopyrightText: Copyright Hamid Jahanzad <Hamid@Jahanzad.com>
// SPDX-FileCopyrightText: Copyright Contributors to the Exchange8:SOR
// SPDX-License-Identifier: Apache-2.0

//! Exchange8:SOR service binary - smart order routing.
//!
//! Design: `services/sor/DESIGN.md`. Core logic in the sibling modules;
//! this shell wires the tokio HTTP/TLS surface via the platform
//! `sentin8-shell` crate (registration, DB pool, health).

#![allow(dead_code)]

// (see the master design, sizing/allocation chapter). Workers (bus
// consumers, session supervisors, purge jobs) are in-process tokio
mod openapi;
mod registration;
mod routing;
mod sor;
mod venue_sessions;

fn extend_router(
    app: axum::Router<std::sync::Arc<sentin8::shell::AppState>>,
    _state: std::sync::Arc<sentin8::shell::AppState>,
) -> axum::Router<std::sync::Arc<sentin8::shell::AppState>> {
    // ── Sentin8:Policy admin API (shared implementation; the shell
    // AppState already implements HasPolicyAdmin) - see DESIGN.md §2.
    sentin8::shell::mount_policy_admin(
        // Outbound venue FIX sessions are dedicated tasks (see DESIGN.md
        // §2); the HTTP router carries health + admin endpoints only.
        app,
    )
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    sentin8::shell::serve(
        sentin8::shell::Sentin8Shell::new("exchange8:sor", "EXCHANGE8_SOR")
            .with_registration(registration::print)
            .with_simple_state()
            .with_router(extend_router),
    )
    .await
}
