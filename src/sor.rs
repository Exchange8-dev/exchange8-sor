// SPDX-FileCopyrightText: Copyright Logically Magic Inc. <hello@logicallymagic.com>
// SPDX-FileCopyrightText: Copyright Hamid Jahanzad <Hamid@Jahanzad.com>
// SPDX-FileCopyrightText: Copyright Contributors to the Exchange8:SOR
// SPDX-License-Identifier: Apache-2.0

//! Ex8:SOR strategy-code parser + phase state machine.
//!
//! Strategy code = 9 digits: `[DD][C][LLLL][PP]` — dark option (2), mode
//! selector (1), lit option (4), post option (2).
//!
//! Design: `services/sor/DESIGN.md` §2 (decision modules).

use thiserror::Error;

/// Strategy-code validation failures (the platform spec parity).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Error)]
pub enum StrategyError {
    #[error("strategy code must be exactly 9 digits")]
    NotNineDigits,
    #[error("non-digit character in strategy code")]
    NonDigit,
    #[error("mode selector (digit 3) must be 0-3, got {0}")]
    BadMode(char),
    #[error("mode 0 (none) requires an empty lit token list")]
    LitWithNoMode,
    #[error("unknown {0} option code: {1}")]
    UnknownOption(&'static str, u32),
}

/// Phase a routed order is in (state machine order).
///
/// `Begin` exists for state-machine completeness (the diagram's entry node)
/// and is reachable only through `next_phase` before the first leg reports.
#[allow(dead_code)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Phase {
    Begin,
    /// Dark midpoint pegs, one per active dark venue session.
    Radar,
    /// Parallel IOC sweep at CBBO price across lit venues.
    Spray,
    /// One venue at a time with a price walk toward the limit.
    Sequential,
    /// Resting remainder posted to the post venue or internally.
    Post,
}

/// Route mode from strategy digit 3 (`opt[2]`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RouteMode {
    /// No routing — internal only.
    None,
    /// Spray phases.
    Spray,
    /// Sequential phases.
    Sequential,
    /// Spray then sequential.
    SprayAndSequential,
}

/// A parsed 9-digit strategy code.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StrategyCode {
    /// Raw code digits.
    pub raw: String,
    /// Dark option (digits 1-2) → dark venue token list.
    pub dark_option: u32,
    /// Mode selector (digit 3).
    pub mode: RouteMode,
    /// Lit option (digits 4-7) → lit venue token list.
    pub lit_option: u32,
    /// Post option (digits 8-9) → post venue token.
    pub post_option: u32,
    /// Dark venue tokens for `dark_option` (resolved by the service at startup).
    pub dark_tokens: Vec<String>,
    /// Lit venue tokens for `lit_option`.
    pub lit_tokens: Vec<String>,
    /// Post venue token for `post_option`.
    pub post_token: Option<String>,
}

impl StrategyCode {
    /// Parse a 9-digit strategy code. Token lists are validated against the
    /// registered option tables passed by the caller (token strings keyed by
    /// option code).
    pub fn parse(
        code: &str,
        dark_table: &dyn Fn(u32) -> Option<Vec<String>>,
        lit_table: &dyn Fn(u32) -> Option<Vec<String>>,
        post_table: &dyn Fn(u32) -> Option<String>,
    ) -> Result<StrategyCode, StrategyError> {
        if code.len() != 9 {
            return Err(StrategyError::NotNineDigits);
        }
        if !code.bytes().all(|b| b.is_ascii_digit()) {
            return Err(StrategyError::NonDigit);
        }
        let mode_char = code.as_bytes()[2] as char;
        let mode = match mode_char {
            '0' => RouteMode::None,
            '1' => RouteMode::Spray,
            '2' => RouteMode::Sequential,
            '3' => RouteMode::SprayAndSequential,
            c => return Err(StrategyError::BadMode(c)),
        };
        let dark_option: u32 = code[0..2].parse().unwrap();
        let lit_option: u32 = code[3..7].parse().unwrap();
        let post_option: u32 = code[7..9].parse().unwrap();
        // Resolve all three option codes.
        let dark_tokens =
            dark_table(dark_option).ok_or(StrategyError::UnknownOption("dark", dark_option))?;
        let lit_tokens =
            lit_table(lit_option).ok_or(StrategyError::UnknownOption("lit", lit_option))?;
        let post_token =
            post_table(post_option).ok_or(StrategyError::UnknownOption("post", post_option))?;
        // Mode 0 (none) requires an empty lit token list.
        if mode == RouteMode::None && !lit_tokens.is_empty() {
            return Err(StrategyError::LitWithNoMode);
        }
        Ok(StrategyCode {
            raw: code.to_string(),
            dark_option,
            mode,
            lit_option,
            post_option,
            dark_tokens,
            lit_tokens,
            post_token: if post_option == 0 {
                None
            } else {
                Some(post_token)
            },
        })
    }

    /// True when the strategy has any dark phase.
    pub fn has_dark(&self) -> bool {
        !self.dark_tokens.is_empty()
    }

    /// True when the strategy has any spray phase.
    pub fn has_spray(&self) -> bool {
        self.mode == RouteMode::Spray || self.mode == RouteMode::SprayAndSequential
    }

    /// True when the strategy has any sequential phase.
    pub fn has_sequential(&self) -> bool {
        self.mode == RouteMode::Sequential || self.mode == RouteMode::SprayAndSequential
    }
}

/// Which venue session is available for a token (the service queries the
/// live session registry; tests stub it).
pub type SessionProbe = dyn Fn(&str) -> bool;

/// Per-venue quote snapshot used by SPRAY/SEQUENTIAL sizing decisions.
#[derive(Clone, Copy, Debug, Default)]
pub struct VenueTop {
    /// Top-of-book price on the contra side (None = no quote).
    pub price: Option<i128>,
    /// Top-of-book size (0 when price is None).
    pub size: u64,
}

/// First phase for a routed order given the strategy and market state.
///
/// entry rules:
/// - post-only strategies go straight to POST.
/// - dark configured + CBBO live → RADAR.
/// - spray enabled + CBBO live → SPRAY.
/// - sequential enabled → SEQUENTIAL.
/// - default → POST.
pub fn entry_phase(strat: &StrategyCode, cbbo_live: bool, has_session: &SessionProbe) -> Phase {
    // Post-only strategies post directly.
    if !strat.has_dark() && !strat.has_spray() && !strat.has_sequential() {
        return Phase::Post;
    }
    // RADAR requires a live dark session + a live CBBO.
    if strat.has_dark() && cbbo_live && strat.dark_tokens.iter().any(|t| has_session(t)) {
        return Phase::Radar;
    }
    // SPRAY needs the CBBO to price against.
    if strat.has_spray() && cbbo_live && strat.lit_tokens.iter().any(|t| has_session(t)) {
        return Phase::Spray;
    }
    // SEQUENTIAL can price-walk without an initial CBBO.
    if strat.has_sequential() && strat.lit_tokens.iter().any(|t| has_session(t)) {
        return Phase::Sequential;
    }
    Phase::Post
}

/// Transition after a phase exhausts. Returns the next phase (never loops
/// backward except SEQUENTIAL's own price walk, handled in the state task).
pub fn next_phase(current: Phase, strat: &StrategyCode) -> Phase {
    match current {
        Phase::Begin => Phase::Post,
        Phase::Radar => {
            if strat.has_spray() {
                Phase::Spray
            } else if strat.has_sequential() {
                Phase::Sequential
            } else {
                Phase::Post
            }
        }
        Phase::Spray => {
            if strat.has_sequential() {
                Phase::Sequential
            } else {
                Phase::Post
            }
        }
        Phase::Sequential => Phase::Post,
        Phase::Post => Phase::Post,
    }
}

/// SPRAY leg sizing: per venue, `qty = min(remaining, venue_top.size)`;
/// the leg is skipped when the venue is missing or worse than the CBBO.
///
/// `side_buy` = true for buy orders (contra = asks).
pub fn spray_leg(
    remaining: u64,
    venue_top: VenueTop,
    cbbo_price: i128,
    side_buy: bool,
) -> Option<u64> {
    let price = venue_top.price?;
    // Buy: skip venues whose ask is worse (higher) than the consolidated ask.
    let worse = if side_buy {
        price > cbbo_price
    } else {
        price < cbbo_price
    };
    if worse {
        return None;
    }
    let qty = remaining.min(venue_top.size);
    if qty == 0 {
        return None;
    }
    Some(qty)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tables(
        dark: &[(&str, &[&str])],
        lit: &[(&str, &[&str])],
        post: &[(&str, &str)],
    ) -> (
        impl Fn(u32) -> Option<Vec<String>> + use<>,
        impl Fn(u32) -> Option<Vec<String>> + use<>,
        impl Fn(u32) -> Option<String> + use<>,
    ) {
        let dark: Vec<(u32, Vec<String>)> = dark
            .iter()
            .map(|(c, ts)| {
                (
                    c.parse().unwrap(),
                    ts.iter().map(|s| s.to_string()).collect(),
                )
            })
            .collect();
        let lit: Vec<(u32, Vec<String>)> = lit
            .iter()
            .map(|(c, ts)| {
                (
                    c.parse().unwrap(),
                    ts.iter().map(|s| s.to_string()).collect(),
                )
            })
            .collect();
        let post: Vec<(u32, String)> = post
            .iter()
            .map(|(c, t)| (c.parse().unwrap(), t.to_string()))
            .collect();
        let df = move |code: u32| {
            dark.iter()
                .find(|(c, _)| *c == code)
                .map(|(_, ts)| ts.clone())
        };
        let lf = move |code: u32| {
            lit.iter()
                .find(|(c, _)| *c == code)
                .map(|(_, ts)| ts.clone())
        };
        let pf = move |code: u32| {
            post.iter()
                .find(|(c, _)| *c == code)
                .map(|(_, t)| t.clone())
        };
        (df, lf, pf)
    }

    #[test]
    fn parse_all_nine_positions() {
        // dark=01, mode=3, lit=0001, post=02 → 9 digits.
        let (df, lf, pf) = tables(
            &[("01", &["LX", "CHX"])],
            &[("0001", &["TSE", "CHX"])],
            &[("02", "TSE")],
        );
        let s = StrategyCode::parse("013000102", &df, &lf, &pf).unwrap();
        assert_eq!(s.dark_option, 1);
        assert_eq!(s.mode, RouteMode::SprayAndSequential);
        assert_eq!(s.lit_option, 1);
        assert_eq!(s.post_option, 2);
        assert_eq!(s.dark_tokens, vec!["LX", "CHX"]);
        assert_eq!(s.lit_tokens, vec!["TSE", "CHX"]);
        assert_eq!(s.post_token.as_deref(), Some("TSE"));
        assert!(s.has_dark() && s.has_spray() && s.has_sequential());
    }

    #[test]
    fn rejects_short_and_long() {
        let (df, lf, pf) = tables(&[("00", &[])], &[("0000", &[])], &[("00", "TSE")]);
        assert_eq!(
            StrategyCode::parse("01300010", &df, &lf, &pf).unwrap_err(),
            StrategyError::NotNineDigits
        );
        assert_eq!(
            StrategyCode::parse("0130001023", &df, &lf, &pf).unwrap_err(),
            StrategyError::NotNineDigits
        );
    }

    #[test]
    fn rejects_non_digits() {
        let (df, lf, pf) = tables(&[("00", &[])], &[("0000", &[])], &[("00", "TSE")]);
        assert_eq!(
            StrategyCode::parse("01A000102", &df, &lf, &pf).unwrap_err(),
            StrategyError::NonDigit
        );
    }

    #[test]
    fn rejects_bad_mode() {
        let (df, lf, pf) = tables(&[("00", &[])], &[("0000", &[])], &[("00", "TSE")]);
        assert_eq!(
            StrategyCode::parse("014000102", &df, &lf, &pf).unwrap_err(),
            StrategyError::BadMode('4')
        );
    }

    #[test]
    fn mode_none_requires_empty_lit() {
        let (df, lf, pf) = tables(&[("01", &["LX"])], &[("0001", &["TSE"])], &[("02", "TSE")]);
        // mode 0 + non-empty lit → invalid.
        assert_eq!(
            StrategyCode::parse("010000102", &df, &lf, &pf).unwrap_err(),
            StrategyError::LitWithNoMode
        );
    }

    #[test]
    fn unknown_option_codes_rejected() {
        let (df, lf, pf) = tables(&[("01", &["LX"])], &[("0001", &["TSE"])], &[("02", "TSE")]);
        assert_eq!(
            StrategyCode::parse("993000102", &df, &lf, &pf).unwrap_err(),
            StrategyError::UnknownOption("dark", 99)
        );
        assert_eq!(
            StrategyCode::parse("013999902", &df, &lf, &pf).unwrap_err(),
            StrategyError::UnknownOption("lit", 9999)
        );
        assert_eq!(
            StrategyCode::parse("013000199", &df, &lf, &pf).unwrap_err(),
            StrategyError::UnknownOption("post", 99)
        );
    }

    #[test]
    fn entry_post_only_strategy_goes_to_post() {
        // mode 0, no dark, no lit → post-only.
        let (df, lf, pf) = tables(&[("00", &[])], &[("0000", &[])], &[("02", "TSE")]);
        let s = StrategyCode::parse("000000002", &df, &lf, &pf).unwrap();
        assert_eq!(entry_phase(&s, true, &|_| true), Phase::Post);
    }

    #[test]
    fn entry_dark_with_cbbo_is_radar() {
        let (df, lf, pf) = tables(&[("01", &["LX"])], &[("0001", &["TSE"])], &[("02", "TSE")]);
        let s = StrategyCode::parse("013000102", &df, &lf, &pf).unwrap();
        assert_eq!(entry_phase(&s, true, &|_| true), Phase::Radar);
        // CBBO dead → RADAR and SPRAY both skipped (both price against the
        // CBBO); SEQUENTIAL can price-walk without it.
        assert_eq!(entry_phase(&s, false, &|_| true), Phase::Sequential);
    }

    #[test]
    fn entry_dark_without_session_falls_to_spray() {
        let (df, lf, pf) = tables(&[("01", &["LX"])], &[("0001", &["TSE"])], &[("02", "TSE")]);
        let s = StrategyCode::parse("013000102", &df, &lf, &pf).unwrap();
        // dark session down → spray with live lit session.
        assert_eq!(entry_phase(&s, true, &|t| t != "LX"), Phase::Spray);
    }

    #[test]
    fn entry_sequential_needs_no_cbbo() {
        let (df, lf, pf) = tables(&[("00", &[])], &[("0001", &["TSE"])], &[("02", "TSE")]);
        let s = StrategyCode::parse("002000102", &df, &lf, &pf).unwrap();
        assert_eq!(s.mode, RouteMode::Sequential);
        assert_eq!(entry_phase(&s, false, &|_| true), Phase::Sequential);
    }

    #[test]
    fn transitions_radar_spray_sequential_post() {
        let (df, lf, pf) = tables(&[("01", &["LX"])], &[("0001", &["TSE"])], &[("02", "TSE")]);
        let s = StrategyCode::parse("013000102", &df, &lf, &pf).unwrap();
        assert_eq!(next_phase(Phase::Radar, &s), Phase::Spray);
        assert_eq!(next_phase(Phase::Spray, &s), Phase::Sequential);
        assert_eq!(next_phase(Phase::Sequential, &s), Phase::Post);
    }

    #[test]
    fn transitions_skip_missing_phases() {
        // Sequential-only: no radar, no spray.
        let (df, lf, pf) = tables(&[("00", &[])], &[("0001", &["TSE"])], &[("02", "TSE")]);
        let s = StrategyCode::parse("002000102", &df, &lf, &pf).unwrap();
        assert_eq!(next_phase(Phase::Radar, &s), Phase::Sequential);
        assert_eq!(next_phase(Phase::Spray, &s), Phase::Sequential);
        // Spray-only: no sequential after spray.
        let (df, lf, pf) = tables(&[("00", &[])], &[("0001", &["TSE"])], &[("02", "TSE")]);
        let s = StrategyCode::parse("001000102", &df, &lf, &pf).unwrap();
        assert_eq!(s.mode, RouteMode::Spray);
        assert_eq!(next_phase(Phase::Spray, &s), Phase::Post);
    }

    #[test]
    fn post_is_terminal() {
        let (df, lf, pf) = tables(&[("00", &[])], &[("0000", &[])], &[("02", "TSE")]);
        let s = StrategyCode::parse("000000002", &df, &lf, &pf).unwrap();
        assert_eq!(next_phase(Phase::Post, &s), Phase::Post);
    }

    #[test]
    fn spray_leg_sizes_and_skips() {
        // Buy: venue at CBBO ask or better participates.
        assert_eq!(
            spray_leg(
                500,
                VenueTop {
                    price: Some(100_000),
                    size: 300
                },
                100_000,
                true
            ),
            Some(300)
        );
        // Venue worse than CBBO → skipped.
        assert_eq!(
            spray_leg(
                500,
                VenueTop {
                    price: Some(100_100),
                    size: 300
                },
                100_000,
                true
            ),
            None
        );
        // No quote → skipped.
        assert_eq!(spray_leg(500, VenueTop::default(), 100_000, true), None);
        // Zero top size → skipped.
        assert_eq!(
            spray_leg(
                500,
                VenueTop {
                    price: Some(100_000),
                    size: 0
                },
                100_000,
                true
            ),
            None
        );
        // Remaining smaller than top → take remaining.
        assert_eq!(
            spray_leg(
                100,
                VenueTop {
                    price: Some(100_000),
                    size: 300
                },
                100_000,
                true
            ),
            Some(100)
        );
        // Sell: venue bid at/above CBBO bid participates; below → skipped.
        assert_eq!(
            spray_leg(
                500,
                VenueTop {
                    price: Some(100_000),
                    size: 300
                },
                100_000,
                false
            ),
            Some(300)
        );
        assert_eq!(
            spray_leg(
                500,
                VenueTop {
                    price: Some(99_900),
                    size: 300
                },
                100_000,
                false
            ),
            None
        );
    }
}
