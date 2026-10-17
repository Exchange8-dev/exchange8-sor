# Exchange8:SOR — Business Requirements & Functionality

> Audience: product owners, business analysts, and anyone evaluating what
> Exchange8:SOR **does for the business**. For architecture read `README.md`;
> for wire-level detail read `DESIGN.md`.

## 1. Business Purpose

- Get participants the best execution the market offers: every routed
  order is walked across venues in a deliberate, cost-aware sequence —
  hidden liquidity first when configured, a sweep of lit venues at the
  consolidated best price, a patient one-venue price walk, and a resting
  remainder posted where it has the best chance of working.
- Make venue choice a business decision, not an accident: routing
  charges decide the cheapest-first order of venues, deterministically
  and reproducibly, so execution cost is consistent and explainable.
- Reach every venue on one connection fabric: supervised outbound venue
  sessions (logon, heartbeat, resilience) keep the exchange continuously
  routable to its external markets, with sequence discipline kept
  automatically across reconnects.
- Keep the exchange's exposure to venues honest: every child leg is
  tracked from creation to fill or kill, every venue fill is reconciled
  into the platform's own record (with amendments), and legs that lose
  their acknowledgment are swept on recovery instead of leaking.
- Concentrate execution complexity in one place: participants send one
  order and one compact strategy code; the smart router encodes decades
  of execution policy so order-management systems stay simple.

## 2. Business Scope

### What the service provides

| Capability | Description |
|---|---|
| Strategy-code routing policy | A compact, fully-validated 9-digit code (`[DD][C][LLLL][PP]`) that encodes dark access, routing mode, lit behavior, and post destination per order |
| Phased execution (RADAR → SPRAY → SEQUENTIAL → POST) | Dark midpoint pegs at active dark sessions, a parallel IOC sweep priced at the consolidated best bid/offer, one-venue-at-a-time price walking toward the limit, then a posted rest |
| Charge-sorted venue selection | Cheapest-routing-charge venues first (deterministic tie-breaks), covering the target quantity with the minimal venue set; sessions that are not live are excluded |
| Outbound venue sessions | Per-venue FIX sessions (per market × broker × symbol-slice) supervised end-to-end: logon, heartbeat, probes, reconnect, and automatic resend of unacknowledged messages |
| Routing configuration tables | Character token → market/destination/FIX-tag mappings, marketable option codes (dark/lit/post), connection registry, and symbol-letter market slices |
| Venue-cost model | Banded routing charges per market, price band, and venue that drive the cheapest-first ordering |
| Routed-order ledger | Parent order and child leg state, fill aggregation, kill/remainder release, and reconciliation views (unacked legs, orphans) |
| Routed trade records & amendments | Venue fills recorded per leg with liquidity marker, plus a formal amendment trail for corrections |
| Internal routing home | The remainder can always be posted internally (re-posted to the matching engine through the Gateway re-post path) when a venue is not configured or exhausted |
| Cancel propagation | Client cancels arriving via the Gateway are pushed to outstanding venue legs synchronously, then the remainder is re-posted per policy |
| Admin surfaces | Routed-order blotter with leg drill-in, routing tables, session controls (enable/disable, sequence reset), Shield policy administration, and the declared `routing`/`legs` RFW pages |
| Durable change events | Routing and trade facts delivered durably through the platform event bus for clearing, surveillance, and member notifications |

### What the service does NOT own (business boundary)

| Concern | Owning service |
|---|---|
| Client connectivity, sessions, protocol translation (FIX, OUCH, REST) | Exchange8:Gateway |
| Order-entry validation, rate and credit authorization | Exchange8:Risk |
| Matching, order books, fill allocation (the internal book) | Exchange8:ME |
| Parent order instruction, allocation, drop copy | Exchange8:OMS |
| Venue market data parsing and dissemination | Exchange8:VFeeds |
| Trade clearing, netting, routed-trade mirroring | Exchange8:Clearing |
| Exchange instruments and rules-of-the-road reference data | Exchange8:RefData |
| Pre-trade margining and position risk | Exchange8:Margin |
| FIX protocol codec and session engines (libraries) | `crates/fix`, `crates/fixed` (workspace libraries) |
| Identity, token issuance, secrets | Sentin8:Auth / Sentin8:Secrets |
| Declarative policy engine and its admin surface | Shield (Sentin8:Policy) |
| Durable event fan-out and audit archival | Sentin8:Bus / Sentin8:Sys |

## 3. Business Capabilities

### Routing strategy & policy (per order)

| Capability | Business rule |
|---|---|
| 9-digit strategy code | Every routed order carries one code; it must be exactly 9 digits with resolvable dark, lit, and post option codes — an invalid code routes nothing and is rejected before any venue is touched |
| Mode selector | Digit 3 picks the routing mode: internal only (0), spray (1), sequential (2), or spray+sequential (3); a none-mode order must declare no lit venues |
| Dark access (RADAR) | When the dark option is configured, a live consolidated price exists, and a dark session is up, hidden midpoint pegs are submitted to each active dark venue (per-venue tag dialects preserved) |
| Lit sweep (SPRAY) | With a live consolidated price, IOC legs sweep lit venues priced at the consolidated opposite side; a venue with a missing or worse price is skipped; each venue's leg is sized to what its top of book can take |
| Price walk (SEQUENTIAL) | One venue at a time at the crossing price; when the venue list is exhausted the list restarts at the next tick step toward the limit; reaching the limit terminates the walk |
| Post (POST) | The resting remainder is posted to the configured post venue with per-venue flag dialects, or posted internally (match on the home book) when no post venue is configured |
| Entry discipline | Post-only strategies never route out; phases requiring a reference price wait for a live consolidated price rather than guess; a phase is entered only if at least one of its venues has a live session |
| Repost discipline | A posted internal remainder is re-validated (it can still be rejected, e.g. on a pending cancel) and continues under a new routing leg identity |

### Venue economics & selection

| Capability | Business rule |
|---|---|
| Cheapest-first ordering | Venues are ordered by routing charge (a banded lookup by market, price band, and active flag); equal charges break by venue identifier, so identical inputs always produce identical leg plans |
| Minimal fan-out | The router takes only the venues needed to cover the requested quantity — the cheapest covering set, nothing more |
| Only live venues participate | A venue whose session is not fully-upped is excluded; routing continues on the remaining venues and the phase degrades only when a phase's entire venue set is gone |
| Skip semantics | Session-down and quote-poor venues are skipped without killing the order; the order keeps its remaining quantity for the next phase |
| Trade liquidity class | Venue fills are recorded as added-to or removed-from liquidity, preserving the economics each venue charges for |

### Venue sessions & connectivity

| Capability | Business rule |
|---|---|
| Session supervision | Each outbound venue link is supervised from cold start through logon, heartbeats, probe requests on missed heartbeats, and reconnection after failure |
| Only a fully-up session routes | An order never reaches a venue whose session is logging on, probing, or down |
| Automatic resend | Unacknowledged outbound messages are held per session and replayed in order on reconnect — venue sequence discipline is maintained, not negotiated by hand |
| Credentials by reference | Venue logon credentials are stored as secret references, never in plaintext |
| Session controls | Operations can enable or disable connections and reset venue sequences administratively |
| Symbol partitioning | Venue sessions are keyed per market × broker × first-letter symbol slice, so venue load and configuration are partitionable by symbol |

### Routed-order ledger & reconciliation

| Capability | Business rule |
|---|---|
| Parent-child ledger | Every routed parent tracks its legs (per-venue child order ids `parent-leg-sequence`), with fills aggregating upward and kills releasing unfilled quantity for re-routing |
| Overfill protection | A venue echoing more quantity than the leg carries completes the leg at its true size — duplicated or over-reported echoes can never inflate the parent |
| Kill & re-route | A rejected, cancelled, or lost leg frees its unfilled remainder for the next venue or phase |
| Reconciliation views | Legs routed but never acknowledged (the recovery cancel-sweep set) and legs without a known parent (orphans) are surfaced explicitly for operations |
| Amendment trail | Venue fills can be amended in quantity or price; both versions are retained with who amended and when |
| Crash-safe rebuild | Parent/leg facts persist independently of the running process; a restarted instance rebuilds its working set and sweeps incomplete legs |

### Administration & observability

| Capability | Business rule |
|---|---|
| Routing configuration | Tokens, marketable options, connections, market slices, and venue-charge bands are admin-managed per environment |
| Routed-order blotter | Routed orders and their legs are viewable with drill-in from the admin surface (`routing`, `legs` RFW pages) |
| Session telemetry | Per-session state, in/out sequence state, and live leg load are exposed to operations |
| Policy administration | Shield policy sets governing who may trigger routing decisions and who administers them are managed on the shared policy surface |
| Durable events | Routing lifecycle and trade facts are emitted through the platform event bus idempotently (per change) |

## 4. Business Events (Outbound)

Emitted via the platform event vocabulary
(`exchange8:{service}:{resource}:{operation}`, colons only — the
`exchange8:routing` event family into durable outbox):

| Event family | When it fires |
|---|---|
| Routed-order/leg lifecycle (`exchange8:routing:*`) | A leg is routed, acknowledged, filled, rejected, or killed; the parent advances phase, completes, or is killed |
| Routed trade (`exchange8:routing` trade family) | A venue execution is recorded per leg (quantity, price, liquidity class), plus amendment trail changes |
| Fill proceeds to clearing | Every recorded venue trade is delivered durably so clearing mirrors the routed trade record |

Inbound (consumed): route/reject requests and leg events from
Exchange8:Gateway, venue FIX acknowledgments and execution reports (sessions
driven by the `crates/fix` engine), STAMP CBBO quote frames from
Exchange8:VFeeds, posted-order lifecycle events returning from the internal
re-post path, Shield policy-set updates via the shared policy consumer,
and routing-configuration facts (tokens, options, connections, slices,
charges).

## 5. Business Rules & Invariants

1. **Invalid strategy, no routing.** A strategy code that fails
   validation (length, digits, mode value, unknown option code, non-empty
   lit list under none-mode) is rejected outright and never produces a
   leg.
2. **Only live sessions route.** A venue contributes legs only while its
   session is fully up; anything else is skipped, and the phase moves on
   only after its whole venue set is exhausted.
3. **Cheapest-first, deterministically.** Venue ordering is by routing
   charge with stable tie-breaks; the same inputs always produce the
   same leg plan.
4. **Phases never go backward.** Orders progress through the configured
   subset of RADAR → SPRAY → SEQUENTIAL → POST; entry waits for the
   market context each phase needs (a live consolidated price where
   required) rather than routing blind.
5. **Legs are sized to the venue.** A sweep leg never exceeds what the
   venue's top of book holds, and a venue priced worse than the
   consolidated market is skipped — no chasing inferior prices on the
   sweep.
6. **Parent totals can never exceed the order.** Venue fills are capped
   at the leg remainder; over-reported or duplicated echoes complete the
   leg at its true size without corrupting the parent.
7. **Kills release business quantity.** A killed un-filled leg returns
   its remainder for re-routing; a filled leg cannot be killed; every
   kill leaves the exact remainder, neither more nor less.
8. **No leg without a parent.** Child legs exist only under a routed
   parent; a leg that loses its parent is an operational anomaly
   (orphan view), never normal state.
9. **Unacknowledged legs are swept, never stranded.** On recovery, routed
   but unacknowledged legs form the explicit cancel-sweep set.
10. **Credentials are references, not values.** Venue logon credentials
    exist only as secret references; plaintext credentials are never
    stored.
11. **Internal routing always exists.** When no post venue is configured
    or phases are exhausted, the remainder posts internally via the
    re-post path so an order is never dropped by routing alone.
12. **Authorization is policy-decided.** Declarative Shield policy is the
    only gate for routing actions: no policy set denies all, tenant
    forbids override platform permits, shadow evaluations never block,
    and machine (M2M) credentials are held to exactly the user standard.
13. **Tenant isolation is unconditional.** All durable tables enforce
    row-level security per environment; one tenant's routing
    configuration, routed orders, or trades are never visible to
    another.
14. **Every routing fact is auditable and durable.** Fill, kill, and
    reconciliation-relevant facts ride the local journal and the
    platform event bus idempotently; an outage delays delivery but never
    loses a record.

## 6. Target-User Roles & Use Cases

| Persona | Role | What they do with Exchange8:SOR |
|---|---|---|
| Retail / institutional trader | Order submitter (via broker/OMS) | Sends one order with one strategy code; experiences the router only as better fill outcomes across venues |
| Sell-side broker / execution desk | Execution strategist | Chooses strategy codes to express execution intent (hide first, sweep, walk, post) without building routing infrastructure |
| Buy-side OMS team | System integrator | Integrates once against the code grammar; the router carries venue mechanics, tag dialects, and reconciliation |
| Exchange operations | Market operator | Monitors venue sessions (state, sequences, live load), toggles connections, resets sequences after venue-side incidents |
| Routing / connectivity operations | Venue manager | Maintains routing tokens, options, connections, symbol slices, and venue-charge bands per environment |
| Clearing / back office | Trade processors | Receive routed-trade records with amendment trails; never touch the routing tables directly |
| Compliance & surveillance | Control owners | Reconstruct any routed order end-to-end: parent, legs, venue events, and the amendment trail |
| Product / business owners | Execution-quality owners | Track routing KPIs (leg latency, phase usage, venue reject rates, charge-weighted outcomes) |

## 7. KPIs / Success Criteria

- **Decision latency**: routing decisions complete well inside the sub-
  millisecond budget (< 500 µs in-process decision plus venue round
  trip), so routing adds no perceptible delay to order entry.
- **Execution quality**: phase coverage and charge-sorted selection
  demonstrably capture the best available venue prices (same
  quote/order sequence reproduces the same leg plan — reproducible for
  best-execution review).
- **Leg throughput**: the venue sustains design load (thousands of
  routed orders per environment per day, up to 200 live venue sessions)
  with headroom.
- **Reconciliation coverage**: zero stranded legs — every unacked leg is
  captured by a cancel sweep and every orphan is surfaced to operations.
- **Session availability**: venue sessions reconnect automatically with
  resend replay; routing skips failed venues with no order-level
  failure.
- **Fill integrity**: no double-counted or inflated fill exists in the
  ledger under duplicate or over-reported venue echoes (capping
  verified by test).
- **Event reliability**: every routing/trade fact is delivered durably
  and idempotently to clearing and member-facing consumers; failed
  deliveries are replayable, never dropped.
- **Isolation**: zero cross-environment visibility of routing data (RLS
  matrix green in every suite run).
- **Audit completeness**: every configuration change and privileged
  action lands in platform audit archival with before/after states.

## 8. Dependencies & Reliance

- **Exchange8:Gateway** — the sole routing counterpart: it delivers authorized
  route requests and carries leg events to clients, re-posts internal
  remainders to the matching engine, and propagates client cancels.
- **Exchange8:VFeeds** — the consolidated quote fabric (STAMP CBBO frames) the
  phased pricing depends on; without a live consolidated price, CBBO-
  requiring phases wait rather than guess.
- **External venues (FIX 4.x over mTLS)** — the liquidity counterpart;
  logon credentials are secret references and per-venue tag dialects
  ride routing configuration.
- **Workspace libraries** — `crates/fix` (session engine),
  `crates/fixed` (fixed-point math), `crates/stamp` (feed codec); the
  sor binary reuses them rather than reimplementing protocol layers.
- **Exchange8:ME / Exchange8:Gateway re-post path** — the internal home for the
  resting remainder when no post venue is configured.
- **Exchange8:Clearing** — the consumer of routed-trade events that mirrors
  venue executions; clearing reads events, never this schema.
- **Exchange8:RefData** — instrument identity, tick tables, and market
  structure that the phase walkers and price bands assume.
- **Sentin8:Auth** — all identity issuance/verification; M2M workloads
  (`exchange8:sor`/`sor`, `sor-audit`, `exchange8-sor-tls`), cert-bound
  by default.
- **Shield (Sentin8:Policy)** — the only authorization gate; the seeded
  scope-mirror set is the service's declared contract; the admin surface
  shares the engine cache.
- **Sentin8:Bus / Sentin8:MemBus / Sentin8:Sys** — durable change
  delivery (outbox relay), in-process publication, audit ingest, and the
  TLS hostname resolution owner API.
- **Sentin8:DB (PostgreSQL 18, db `exchange8`, schema `sor`)** — routing
  configuration, routed-order facts, trades and amendments, venue
  charges and slices, outbox, and audit journal; accessed only via the
  service's own role under row-level security.
- **Platform shell (`sentin8::shell`)** — registration, pool, health,
  drain, allocator; `sentin8-tls` for in-process post-quantum TLS and
  venue mTLS.

## 9. Acceptance Criteria

1. A route request carrying a valid strategy code produces the exact
   leg plan the strategy implies: dark legs where configured, a
   charge-cost-minimal lit covering set, sequential walking where
   configured, and the correct post destination for the remainder.
2. An invalid strategy code (wrong length, non-digits, bad mode digit,
   unknown option code, lit tokens under none-mode) is rejected with the
   declared error contract and produces no legs.
3. A venue without a fully-up session receives no leg; routing
   continues on remaining venues and the phase degrades only when its
   venue set is exhausted.
4. Venue legs are submitted with per-venue tag dialects preserved and
   unacknowledged messages replayed in order on reconnect.
5. A missed-heartbeat session is probed before being marked down;
   logon-rejected and disconnected sessions re-enter the reconnect loop
   and only a fully-up session routes again.
6. Fill aggregation is exact: fills aggregate from leg to parent, never
   exceeding the ordered quantity, and duplicates/over-echoes complete
   legs at their true size.
7. A killed leg releases exactly its unfilled remainder for re-routing;
   killing an already-filled leg changes nothing.
8. Cancel propagation reaches every outstanding venue leg synchronously,
   and the remainder is re-posted per policy (including internal
   re-posting with re-validation and reject-on-pending-cancel).
9. Reconciliation after a restart identifies every unacknowledged leg
   (cancel-sweep set) and every orphaned leg as explicit anomalies;
   incomplete legs are reconciled by the venue sweep.
10. Routed trades and their amendments are recorded with liquidity
    class and delivered durably to clearing; no consumer reads the
    routing tables directly.
11. Every durable table is RLS-forced with provable environment
    isolation (no context → zero rows; cross-env → zero rows; contextless
    insert → rejected), and the service role holds no bypass privileges.
12. Shield's seeded policy is the only authorization gate for routing
    actions: missing or non-matching sets deny (also for machine tokens),
    shadow never blocks, tenant forbids win, and platform-admin scopes
    pass.
13. Identical quote/order sequences produce identical leg plans
    (deterministic charge-sorted selection), reproducible for
    best-execution evidence.
14. Rolling instance upgrades complete with graceful drain; no in-flight
    routing decision and no outbox/journal record is lost across the
    upgrade boundary.

## 10. Open Business Questions

- Routing-charge banding values (the charge table per market and price
  band, and any dynamic book-charge tiers beyond the banded lookup) are
  business pricing policy; the mechanism and its determinism are final.
- The public business payloads and consumer contract for
  `exchange8:routing:*` lifecycle and trade events (clearing and
  surveillance consumers) are being finalized with the receiving teams.
- The member-facing rulebook division of responsibility between the
  gateway's order validation and this service's strategy-code rejection
  surface (which codes members may receive from either surface).
- Session-control governance: who may enable/disable venue connections
  or reset venue sequences (admin-scope assignment and approval
  workflow) is a business-governance decision.
- Default strategy-code catalog per participant class (which of the
  dark/spray/sequential/post combinations are offered to which member
  tiers) is a product decision atop the grammar.
- Execution-quality reporting derived from this service's facts (phase
  usage, charge-weighted venue outcomes, staleness windows) is a
  product/analytics surface to define with market-data owners.

---

**Design attribution:** exchange8:sor and its underlying concepts were designed by Hamid Jahanzad <Hamid@Jahanzad.com>.
