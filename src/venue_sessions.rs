// SPDX-FileCopyrightText: Copyright Logically Magic Inc. <hello@logicallymagic.com>
// SPDX-FileCopyrightText: Copyright Hamid Jahanzad <Hamid@Jahanzad.com>
// SPDX-FileCopyrightText: Copyright Contributors to the Exchange8:SOR
// SPDX-License-Identifier: Apache-2.0

//! Exchange8:SOR outbound venue sessions + VFeeds STAMP CBBO adapter.
//!
//! - **Venue sessions**: outbound venue FIX sessions — the session
//!   supervisor state (per-venue logon/heartbeat/test-request/queue
//!   states) + the resend buffer the `crates/fix` engine drives. Venue
//!   down → skip (routing continues on remaining venues, DESIGN.md §6).
//! - **STAMP CBBO**: TSX consolidated BBO packets from the STAMP feed:
//!   text frames `CBBO|inst|bidpx|bidsize|askpx|asksize` (the STAMP codec
//!   in `crates/stamp` owns framing; this adapter normalizes to
//!   `VenueQuoteEvent`; see DESIGN.md §2).
//!
//! Design: `services/sor/DESIGN.md` §2, §6
//! (feed codec contract in the venue-feeds design doc).

use std::collections::HashMap;

// ---------------------------------------------------------------------------
// venue session supervisor (DESIGN.md §2)
// ---------------------------------------------------------------------------

/// Per-venue session state.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum VenueSessionState {
    /// Never connected / cold start.
    Idle,
    /// TCP up, logon in flight.
    LoggingOn,
    /// Logged on, heartbeats flowing.
    Up,
    /// Missed heartbeat window — test-request probe in flight.
    Probing,
    /// Down (logon rejected / disconnect / probe failed) — routing skips.
    Down,
}

/// Events the supervisor consumes.
#[derive(Clone, Debug, PartialEq)]
pub enum VenueSessionEvent {
    Connect,
    LogonAccepted,
    LogonRejected,
    Heartbeat,
    HeartbeatTimeout,
    TestRequestOk,
    Disconnect,
}

/// The supervisor: one state machine per venue MIC.
pub struct VenueSessions {
    states: HashMap<String, VenueSessionState>,
}

impl VenueSessions {
    pub fn new(venues: &[&str]) -> Self {
        let mut states = HashMap::new();
        for v in venues {
            states.insert((*v).to_string(), VenueSessionState::Idle);
        }
        VenueSessions { states }
    }

    pub fn state(&self, venue: &str) -> VenueSessionState {
        self.states
            .get(venue)
            .copied()
            .unwrap_or(VenueSessionState::Down) // unknown venue never routes
    }

    /// Is the venue routable right now?
    pub fn routable(&self, venue: &str) -> bool {
        self.state(venue) == VenueSessionState::Up
    }

    /// Apply one event; returns the new state.
    pub fn on_event(&mut self, venue: &str, ev: VenueSessionEvent) -> VenueSessionState {
        use VenueSessionEvent as E;
        use VenueSessionState as S;
        let next = match (self.state(venue), ev) {
            (S::Idle, E::Connect) => S::LoggingOn,
            (S::LoggingOn, E::LogonAccepted) => S::Up,
            (S::LoggingOn, E::LogonRejected) => S::Down,
            (S::Up, E::Heartbeat) => S::Up,
            (S::Up, E::HeartbeatTimeout) => S::Probing,
            (S::Probing, E::TestRequestOk) => S::Up,
            (S::Probing, E::HeartbeatTimeout) | (S::Probing, E::Disconnect) => S::Down,
            (S::Up, E::Disconnect) => S::Down,
            (S::Down, E::Connect) => S::LoggingOn, // reconnect loop
            // Idempotent no-ops (duplicate events, replays).
            (s, _) => s,
        };
        if let Some(st) = self.states.get_mut(venue) {
            *st = next;
        }
        next
    }

    /// Venue set snapshot (routing input).
    pub fn venues(&self) -> Vec<(&str, VenueSessionState)> {
        let mut v: Vec<_> = self.states.iter().map(|(k, s)| (k.as_str(), *s)).collect();
        v.sort();
        v
    }
}

/// The outbound resend buffer per venue (MsgSaver parity): messages held
/// until venue-acked, replayed on reconnect.
pub struct ResendBuffer {
    /// (seq, message bytes) per venue, in order.
    buffers: HashMap<String, Vec<(u64, Vec<u8>)>>,
}

impl ResendBuffer {
    pub fn new(venues: &[&str]) -> Self {
        let mut buffers = HashMap::new();
        for v in venues {
            buffers.insert((*v).to_string(), Vec::new());
        }
        ResendBuffer { buffers }
    }

    /// Hold one message until acked.
    pub fn hold(&mut self, venue: &str, seq: u64, msg: Vec<u8>) {
        self.buffers
            .entry(venue.to_string())
            .or_default()
            .push((seq, msg));
    }

    /// Venue acked through `seq` — drop covered messages.
    pub fn on_ack(&mut self, venue: &str, seq: u64) {
        if let Some(buf) = self.buffers.get_mut(venue) {
            buf.retain(|(s, _)| *s > seq);
        }
    }

    /// Replay set on reconnect (unacked, in order).
    pub fn replay(&self, venue: &str) -> Vec<(u64, Vec<u8>)> {
        self.buffers.get(venue).cloned().unwrap_or_default()
    }
}

// ---------------------------------------------------------------------------
// STAMP CBBO adapter (DESIGN.md §2)
// ---------------------------------------------------------------------------

/// Normalized quote event (vfeeds adapters.rs shape, restated locally for
/// the stamp adapter module's self-containment — the shell unifies).
#[derive(Clone, Debug, PartialEq)]
pub struct StampQuote {
    pub instrument_id: u32,
    /// Consolidated bid (price ×10⁴, size).
    pub bid: Option<(i128, u64)>,
    /// Consolidated ask.
    pub ask: Option<(i128, u64)>,
    /// Venue sequence when present.
    pub seq: u64,
}

/// The STAMP CBBO adapter: text frames → normalized consolidated quotes.
pub struct StampCbboAdapter {
    /// Transport identity (multicast A/B legs configured by the shell).
    pub feed_id: u64,
}

impl StampCbboAdapter {
    pub fn new(feed_id: u64) -> Self {
        StampCbboAdapter { feed_id }
    }

    /// Parse one frame of newline-separated CBBO records:
    /// `CBBO|inst|bidpx|bidsize|askpx|asksize[|seq]`.
    pub fn parse(&self, frame: &[u8]) -> Vec<StampQuote> {
        let text = String::from_utf8_lossy(frame);
        let mut out = Vec::new();
        for line in text.lines() {
            let f: Vec<&str> = line.split('|').collect();
            if f.len() < 6 || f[0] != "CBBO" {
                continue;
            }
            let inst: u32 = f[1].parse().unwrap_or(0);
            let bid = f[2].parse::<i128>().ok().filter(|p| *p > 0);
            let ask = f[4].parse::<i128>().ok().filter(|p| *p > 0);
            out.push(StampQuote {
                instrument_id: inst,
                bid: bid.map(|p| (p, f[3].parse().unwrap_or(0))),
                ask: ask.map(|p| (p, f[5].parse().unwrap_or(0))),
                seq: f.get(6).and_then(|s| s.parse().ok()).unwrap_or(0),
            });
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use exchange8::fixed::Qty;

    #[test]
    fn venue_session_lifecycle() {
        let mut s = VenueSessions::new(&["XTSE", "CHIC"]);
        assert_eq!(s.state("XTSE"), VenueSessionState::Idle);
        assert!(!s.routable("XTSE"));
        // Cold start → logon → up.
        s.on_event("XTSE", VenueSessionEvent::Connect);
        assert_eq!(s.state("XTSE"), VenueSessionState::LoggingOn);
        s.on_event("XTSE", VenueSessionEvent::LogonAccepted);
        assert_eq!(s.state("XTSE"), VenueSessionState::Up);
        assert!(s.routable("XTSE"));
        // Heartbeat timeout → probe → ok.
        s.on_event("XTSE", VenueSessionEvent::HeartbeatTimeout);
        assert_eq!(s.state("XTSE"), VenueSessionState::Probing);
        s.on_event("XTSE", VenueSessionEvent::TestRequestOk);
        assert!(s.routable("XTSE"));
        // Probe failure → down; reconnect works.
        s.on_event("XTSE", VenueSessionEvent::HeartbeatTimeout);
        s.on_event("XTSE", VenueSessionEvent::Disconnect);
        assert_eq!(s.state("XTSE"), VenueSessionState::Down);
        s.on_event("XTSE", VenueSessionEvent::Connect);
        assert_eq!(s.state("XTSE"), VenueSessionState::LoggingOn);
    }

    #[test]
    fn logon_rejection_marks_down() {
        let mut s = VenueSessions::new(&["AEQA"]);
        s.on_event("AEQA", VenueSessionEvent::Connect);
        s.on_event("AEQA", VenueSessionEvent::LogonRejected);
        assert_eq!(s.state("AEQA"), VenueSessionState::Down);
        assert!(!s.routable("AEQA"));
    }

    #[test]
    fn unknown_venue_never_routable() {
        let s = VenueSessions::new(&["XTSE"]);
        assert_eq!(s.state("GHOST"), VenueSessionState::Down);
        assert!(!s.routable("GHOST"));
    }

    #[test]
    fn duplicate_events_are_idempotent() {
        let mut s = VenueSessions::new(&["XTSE"]);
        s.on_event("XTSE", VenueSessionEvent::Connect);
        s.on_event("XTSE", VenueSessionEvent::LogonAccepted);
        // Duplicate logon-accepted: stays Up.
        s.on_event("XTSE", VenueSessionEvent::LogonAccepted);
        assert_eq!(s.state("XTSE"), VenueSessionState::Up);
    }

    #[test]
    fn resend_buffer_holds_acks_and_replays() {
        let mut b = ResendBuffer::new(&["XTSE"]);
        b.hold("XTSE", 1, vec![1]);
        b.hold("XTSE", 2, vec![2]);
        b.hold("XTSE", 3, vec![3]);
        // Ack through 2 → only seq 3 remains.
        b.on_ack("XTSE", 2);
        let replay = b.replay("XTSE");
        assert_eq!(replay.len(), 1);
        assert_eq!(replay[0].0, 3);
        // Other venue empty.
        assert!(b.replay("CHIC").is_empty());
    }

    #[test]
    fn stamp_cbbo_parses_quotes() {
        let a = StampCbboAdapter::new(1);
        let frame = b"CBBO|7|99000|100|101000|200|55\nCBBO|8|0|0|105000|300|56\nIGNORE\n";
        let quotes = a.parse(frame);
        assert_eq!(quotes.len(), 2);
        assert_eq!(quotes[0].instrument_id, 7);
        assert_eq!(quotes[0].bid, Some((99_000, 100)));
        assert_eq!(quotes[0].ask, Some((101_000, 200)));
        assert_eq!(quotes[0].seq, 55);
        // Zero bid = no bid side.
        assert_eq!(quotes[1].bid, None);
        assert_eq!(quotes[1].ask, Some((105_000, 300)));
    }

    #[test]
    fn stamp_cbbo_skips_malformed() {
        let a = StampCbboAdapter::new(1);
        assert!(a.parse(b"NOTCBBO|1\nCBBO|short\n").is_empty());
        assert!(a.parse(b"").is_empty());
    }

    #[test]
    fn qty_type_available() {
        // Qty is used by the shell for sizes; keep the import honest.
        let q = Qty::units(100);
        assert_eq!(q.raw, 100);
    }
}
