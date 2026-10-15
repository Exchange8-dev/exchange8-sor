// SPDX-FileCopyrightText: Copyright Logically Magic Inc. <hello@logicallymagic.com>
// SPDX-FileCopyrightText: Copyright Hamid Jahanzad <Hamid@Jahanzad.com>
// SPDX-FileCopyrightText: Copyright Contributors to the Exchange8:SOR
// SPDX-License-Identifier: Apache-2.0

//! Exchange8:SOR routing internals (see DESIGN.md §2–§3).
//!
//! - **Phase machine**: the full RADAR → SPRAY → SEQUENTIAL → POST arc
//!   with venue-failure transitions.
//! - **Venue-charge-sorted selection**: stable sort by venue charge
//!   (ties broken by venue id — deterministic routing).
//! - **Routed-order ledger + leg reconciliation**: parent/leg state
//!   tracking, fill aggregation, orphans (legs without a parent) and
//!   unacked legs (cancel sweep on recovery).
//!
//! Design: `services/sor/DESIGN.md` §2, §3, §6.

use std::collections::HashMap;

use exchange8::fixed::Qty;

/// SOR phase (matches sor.rs `Phase` — restated for the ledger's own
/// state machine with failure transitions).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SorPhase {
    Radar,
    Spray,
    Sequential,
    Post,
    /// Terminal: fully filled.
    Complete,
    /// Terminal: killed / timeout / exhausted.
    Killed,
}

impl SorPhase {
    /// Normal phase advance.
    pub fn advance(self) -> SorPhase {
        match self {
            SorPhase::Radar => SorPhase::Spray,
            SorPhase::Spray => SorPhase::Sequential,
            SorPhase::Sequential => SorPhase::Post,
            SorPhase::Post | SorPhase::Complete | SorPhase::Killed => SorPhase::Killed,
        }
    }

    /// Venue-session failure transition (see DESIGN.md §6 — "skip venue";
    /// the phase itself degrades only when the phase's venue set is
    /// exhausted).
    pub fn on_venue_failure(self, remaining_venues: usize) -> SorPhase {
        if remaining_venues == 0 {
            self.advance()
        } else {
            self
        }
    }
}

// ---------------------------------------------------------------------------
// venue-charge-sorted selection (DESIGN.md §2)
// ---------------------------------------------------------------------------

/// One venue's routing candidacy.
#[derive(Clone, Debug, PartialEq)]
pub struct VenueCandidate {
    pub venue: String,
    /// Routing charge (raw; lower = better).
    pub charge: i128,
    /// Venue session up.
    pub up: bool,
    /// Fillable qty at the venue (from the quote/depth snapshot).
    pub fillable: Qty,
}

/// Charge-sorted venue selection: filter up-venues with fillable
/// qty, stable sort by (charge, venue) — deterministic and
/// charge-minimal. Ties break by venue id (stable order across runs).
pub fn select_venues(candidates: &[VenueCandidate], need: Qty) -> Vec<&VenueCandidate> {
    let mut eligible: Vec<&VenueCandidate> = candidates
        .iter()
        .filter(|c| c.up && c.fillable.raw > 0)
        .collect();
    // Stable sort by charge; ties by venue id for determinism.
    eligible.sort_by(|a, b| (a.charge, &a.venue).cmp(&(b.charge, &b.venue)));
    // Take venues until `need` is covered (SPRAY fan-out set).
    let mut covered = 0u64;
    let mut out = Vec::new();
    for c in eligible {
        out.push(c);
        covered += c.fillable.raw;
        if covered >= need.raw {
            break;
        }
    }
    out
}

// ---------------------------------------------------------------------------
// routed-order ledger + leg reconciliation (DESIGN.md §2–§3)
// ---------------------------------------------------------------------------

/// Leg lifecycle state.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LegState {
    /// Routed, not yet venue-acked.
    Pending,
    /// Venue-accepted, working.
    Working,
    /// Fully filled.
    Filled,
    /// Killed/cancelled/rejected.
    Killed,
}

/// One routed leg.
#[derive(Clone, Debug, PartialEq)]
pub struct RoutedLeg {
    pub leg_id: u64,
    pub parent_id: u64,
    pub venue: String,
    pub qty: Qty,
    pub filled: Qty,
    pub state: LegState,
}

/// One routed parent order.
#[derive(Clone, Debug, PartialEq)]
pub struct RoutedOrder {
    pub parent_id: u64,
    pub qty: Qty,
    pub filled: Qty,
    pub phase: SorPhase,
}

/// The routed-order ledger: parents + legs + reconciliation views.
pub struct RoutedLedger {
    parents: HashMap<u64, RoutedOrder>,
    legs: HashMap<u64, RoutedLeg>,
    next_leg_id: u64,
}

impl RoutedLedger {
    pub fn new() -> Self {
        RoutedLedger {
            parents: HashMap::new(),
            legs: HashMap::new(),
            next_leg_id: 1,
        }
    }

    /// Open a routed parent.
    pub fn open_parent(&mut self, parent_id: u64, qty: Qty) {
        self.parents.insert(
            parent_id,
            RoutedOrder {
                parent_id,
                qty,
                filled: Qty::ZERO,
                phase: SorPhase::Radar,
            },
        );
    }

    /// Route one leg of a parent; returns the leg id.
    pub fn route_leg(&mut self, parent_id: u64, venue: &str, qty: Qty) -> Option<u64> {
        if !self.parents.contains_key(&parent_id) {
            return None;
        }
        let leg_id = self.next_leg_id;
        self.next_leg_id += 1;
        self.legs.insert(
            leg_id,
            RoutedLeg {
                leg_id,
                parent_id,
                venue: venue.into(),
                qty,
                filled: Qty::ZERO,
                state: LegState::Pending,
            },
        );
        Some(leg_id)
    }

    /// Venue ack.
    pub fn on_leg_ack(&mut self, leg_id: u64) -> bool {
        match self.legs.get_mut(&leg_id) {
            Some(l) if l.state == LegState::Pending => {
                l.state = LegState::Working;
                true
            }
            _ => false,
        }
    }

    /// Leg fill (partial or full); aggregates to the parent.
    /// Returns (leg_complete, parent_complete).
    pub fn on_leg_fill(&mut self, leg_id: u64, qty: Qty) -> Option<(bool, bool)> {
        let leg = self.legs.get_mut(&leg_id)?;
        let take = qty.raw.min(leg.qty.raw.saturating_sub(leg.filled.raw));
        leg.filled = Qty::units(leg.filled.raw + take);
        let leg_done = leg.filled.raw >= leg.qty.raw;
        if leg_done {
            leg.state = LegState::Filled;
        }
        let parent_id = leg.parent_id;
        let parent_done = {
            let p = self.parents.get_mut(&parent_id)?;
            p.filled = Qty::units(p.filled.raw + take);
            if p.filled.raw >= p.qty.raw {
                p.phase = SorPhase::Complete;
                true
            } else {
                false
            }
        };
        Some((leg_done, parent_done))
    }

    /// Leg killed (venue reject/cancel); frees the qty for re-routing.
    pub fn on_leg_killed(&mut self, leg_id: u64) -> Option<(u64, Qty)> {
        let leg = self.legs.get_mut(&leg_id)?;
        if leg.state == LegState::Filled {
            return None;
        }
        let remaining = Qty::units(leg.qty.raw - leg.filled.raw);
        leg.state = LegState::Killed;
        let parent_id = leg.parent_id;
        Some((parent_id, remaining))
    }

    /// Parent phase advance.
    pub fn advance_phase(&mut self, parent_id: u64) -> Option<SorPhase> {
        let p = self.parents.get_mut(&parent_id)?;
        p.phase = p.phase.advance();
        Some(p.phase)
    }

    /// Reconciliation (recovery views — see DESIGN.md §6): unacked legs —
    /// the cancel sweep set; filled-but-unparented — orphans.
    pub fn reconcile(&self) -> (Vec<u64>, Vec<u64>) {
        let unacked: Vec<u64> = self
            .legs
            .values()
            .filter(|l| l.state == LegState::Pending)
            .map(|l| l.leg_id)
            .collect();
        let orphaned: Vec<u64> = self
            .legs
            .values()
            .filter(|l| !self.parents.contains_key(&l.parent_id))
            .map(|l| l.leg_id)
            .collect();
        (unacked, orphaned)
    }

    pub fn parent(&self, parent_id: u64) -> Option<&RoutedOrder> {
        self.parents.get(&parent_id)
    }

    pub fn leg(&self, leg_id: u64) -> Option<&RoutedLeg> {
        self.legs.get(&leg_id)
    }
}

impl Default for RoutedLedger {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cand(venue: &str, charge: i128, up: bool, fillable: u64) -> VenueCandidate {
        VenueCandidate {
            venue: venue.into(),
            charge,
            up,
            fillable: Qty::units(fillable),
        }
    }

    #[test]
    fn phase_arc_advances() {
        assert_eq!(SorPhase::Radar.advance(), SorPhase::Spray);
        assert_eq!(SorPhase::Spray.advance(), SorPhase::Sequential);
        assert_eq!(SorPhase::Sequential.advance(), SorPhase::Post);
        assert_eq!(SorPhase::Post.advance(), SorPhase::Killed);
        // Terminal stays terminal.
        assert_eq!(SorPhase::Complete.advance(), SorPhase::Killed);
    }

    #[test]
    fn venue_failure_advances_only_when_exhausted() {
        // Venues remain → stay.
        assert_eq!(SorPhase::Radar.on_venue_failure(2), SorPhase::Radar);
        // None remain → degrade.
        assert_eq!(SorPhase::Spray.on_venue_failure(0), SorPhase::Sequential);
    }

    #[test]
    fn selection_sorts_by_charge_then_venue() {
        let cands = vec![
            cand("CHIC", 30, true, 100),
            cand("AEQA", 10, true, 100),
            cand("XTSE", 20, true, 100),
            cand("DOWN", 1, false, 100), // session down — excluded
            cand("EMPTY", 1, true, 0),   // no fillable — excluded
        ];
        let sel = select_venues(&cands, Qty::units(250));
        let venues: Vec<&str> = sel.iter().map(|c| c.venue.as_str()).collect();
        // Charge-sorted: AEQA(10) → XTSE(20) → CHIC(30); covered at 300 ≥ 250.
        assert_eq!(venues, vec!["AEQA", "XTSE", "CHIC"]);
    }

    #[test]
    fn selection_tie_breaks_by_venue_id() {
        let cands = vec![
            cand("ZZZE", 10, true, 100),
            cand("AAAF", 10, true, 100),
            cand("MMM", 10, true, 100),
        ];
        let sel = select_venues(&cands, Qty::units(300));
        let venues: Vec<&str> = sel.iter().map(|c| c.venue.as_str()).collect();
        assert_eq!(venues, vec!["AAAF", "MMM", "ZZZE"]);
    }

    #[test]
    fn selection_stops_when_covered() {
        let cands = vec![cand("A", 10, true, 400), cand("B", 20, true, 100)];
        let sel = select_venues(&cands, Qty::units(300));
        // Venue A alone covers 300.
        assert_eq!(sel.len(), 1);
        assert_eq!(sel[0].venue, "A");
    }

    #[test]
    fn ledger_routes_fills_and_completes() {
        let mut led = RoutedLedger::new();
        led.open_parent(1, Qty::units(500));
        let l1 = led.route_leg(1, "XTSE", Qty::units(300)).unwrap();
        let l2 = led.route_leg(1, "CHIC", Qty::units(200)).unwrap();
        assert!(led.on_leg_ack(l1));
        assert!(led.on_leg_ack(l2));
        // Partial fill 250 on leg 1.
        let (leg_done, parent_done) = led.on_leg_fill(l1, Qty::units(250)).unwrap();
        assert!(!leg_done && !parent_done);
        // Complete leg 1.
        let (leg_done, _) = led.on_leg_fill(l1, Qty::units(50)).unwrap();
        assert!(leg_done);
        assert_eq!(led.leg(l1).unwrap().state, LegState::Filled);
        // Complete the parent via leg 2.
        let (_, parent_done) = led.on_leg_fill(l2, Qty::units(200)).unwrap();
        assert!(parent_done);
        assert_eq!(led.parent(1).unwrap().phase, SorPhase::Complete);
        assert_eq!(led.parent(1).unwrap().filled.raw, 500);
    }

    #[test]
    fn killed_leg_frees_remaining() {
        let mut led = RoutedLedger::new();
        led.open_parent(1, Qty::units(300));
        let l1 = led.route_leg(1, "XTSE", Qty::units(300)).unwrap();
        led.on_leg_ack(l1);
        led.on_leg_fill(l1, Qty::units(100));
        // Venue kills it → 200 free for re-route.
        let (parent, remaining) = led.on_leg_killed(l1).unwrap();
        assert_eq!(parent, 1);
        assert_eq!(remaining.raw, 200);
        assert_eq!(led.leg(l1).unwrap().state, LegState::Killed);
        // Killing a filled leg is a no-op.
        let mut led2 = RoutedLedger::new();
        led2.open_parent(2, Qty::units(100));
        let l2 = led2.route_leg(2, "XTSE", Qty::units(100)).unwrap();
        led2.on_leg_fill(l2, Qty::units(100));
        assert!(led2.on_leg_killed(l2).is_none());
    }

    #[test]
    fn overfill_is_capped_at_leg_qty() {
        let mut led = RoutedLedger::new();
        led.open_parent(1, Qty::units(100));
        let l1 = led.route_leg(1, "XTSE", Qty::units(100)).unwrap();
        // Venue echoes 150 on a 100 leg (drop-detect case) — cap at 100.
        let (leg_done, parent_done) = led.on_leg_fill(l1, Qty::units(150)).unwrap();
        assert!(leg_done && parent_done);
        assert_eq!(led.parent(1).unwrap().filled.raw, 100);
    }

    #[test]
    fn reconcile_finds_unacked_and_orphans() {
        let mut led = RoutedLedger::new();
        led.open_parent(1, Qty::units(300));
        let l1 = led.route_leg(1, "XTSE", Qty::units(100)).unwrap();
        let _l2 = led.route_leg(1, "CHIC", Qty::units(100)).unwrap();
        let l3 = led.route_leg(1, "AEQA", Qty::units(100)).unwrap();
        // l1 acked; l2/l3 pending; l3's parent removed (crash simulation).
        assert!(led.on_leg_ack(l1));
        let (unacked, orphans) = led.reconcile();
        assert_eq!(unacked.len(), 2); // l2, l3
        assert!(orphans.is_empty());
        // Simulate parent loss → l3 orphaned.
        led.parents.remove(&1);
        let (unacked, orphans) = led.reconcile();
        assert!(unacked.contains(&l3));
        assert!(orphans.contains(&l3));
        let _ = l1;
    }

    #[test]
    fn leg_for_unknown_parent_rejected() {
        let mut led = RoutedLedger::new();
        assert!(led.route_leg(99, "XTSE", Qty::units(10)).is_none());
    }
}
