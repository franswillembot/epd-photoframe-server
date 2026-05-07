use std::collections::HashSet;

use chrono::{DateTime, Duration, Offset, Utc};
use chrono_tz::Tz;
use rand::rngs::StdRng;
use rand::{Rng, SeedableRng, seq::SliceRandom};

use crate::config::{Rotate, ScreenConfig};

impl Rotate {
    /// Next scheduled trigger strictly after `after`, in UTC.
    ///
    /// Both parsers have their own timezone story:
    /// - `cron` consumes a `DateTime<Tz>` and returns triggers in that TZ.
    /// - `cron-lingo` only iterates from the system clock (no arbitrary start);
    ///   we use its `assume_offset` to pin the offset and rely on the fact that
    ///   we only ever call this with `after ≈ now`.
    pub fn next_after(&self, after: DateTime<Utc>, tz: &Tz) -> Option<DateTime<Utc>> {
        match self {
            Self::Cron(s) => s
                .after(&after.with_timezone(tz))
                .next()
                .map(|dt| dt.with_timezone(&Utc)),
            Self::Natural(s) => {
                let offset_secs = after.with_timezone(tz).offset().fix().local_minus_utc();
                let offset = time::UtcOffset::from_whole_seconds(offset_secs).ok()?;
                let next = s
                    .iter()
                    .inspect_err(|e| tracing::warn!(error = ?e, "cron-lingo iter failed"))
                    .ok()?
                    .assume_offset(offset)
                    .next()?
                    .ok()?;
                let utc = next.to_offset(time::UtcOffset::UTC);
                DateTime::<Utc>::from_timestamp(utc.unix_timestamp(), utc.nanosecond())
            }
        }
    }
}

/// Per-screen rotation state. Owns the rotation schedule and timezone so
/// callers don't have to thread them through every operation. `next_rotation`
/// is a tristate encoded in `Option<DateTime<Utc>>`:
/// - `Some(DateTime::<Utc>::MIN_UTC)` — rotate ASAP (the constructor sets
///   this; the first call to `maybe_rotate` always fires, regardless of
///   schedule, so the placeholder `seed = 0` gets replaced with a real one).
/// - `Some(t)` — the next trigger is at moment `t`. Computed at the moment
///   rotation fires; the `cron-lingo` backend can only produce
///   "next-after-current-system-time", so we have to capture the value when
///   `now ≈ system clock`, not derive it later from a stored last-rotation.
/// - `None` — never rotate again (no schedule, or schedule has no future
///   triggers). Reached after the first rotation on a screen with no
///   `rotate` config, or when a one-shot schedule has run out.
pub struct ScreenState {
    seed: u64,
    cursor: i64,
    next_rotation: Option<DateTime<Utc>>,
    rotate: Option<Rotate>,
    tz: Tz,
}

impl ScreenState {
    pub fn new(config: &ScreenConfig) -> Self {
        Self {
            seed: 0,
            cursor: 0,
            next_rotation: Some(DateTime::<Utc>::MIN_UTC),
            rotate: config.rotate.clone(),
            tz: config.timezone,
        }
    }

    pub fn seed(&self) -> u64 {
        self.seed
    }

    pub fn cursor(&self) -> i64 {
        self.cursor
    }

    fn advance(&mut self, delta: i64) {
        self.cursor = self.cursor.wrapping_add(delta);
    }

    /// The next scheduled rotation moment from `now`'s perspective. The
    /// cached `next_rotation` is the source of truth — when it's in the
    /// past (the ASAP sentinel after init, or a missed trigger) we return
    /// `now` so the device wakes immediately and `maybe_rotate` fires on
    /// the next request. `None` (no schedule, or schedule expired) passes
    /// through. Calculation of the next trigger lives only in
    /// `maybe_rotate`.
    pub fn next_scheduled_rotation(&self, now: DateTime<Utc>) -> Option<DateTime<Utc>> {
        self.next_rotation.map(|t| t.max(now))
    }

    /// Apply rotation, navigation, and (when triggered) snap-to-new in one
    /// atomic state transition, then return the resolved photo index.
    /// `advance` shifts the cursor (1 for next, -1 for previous, 0 for
    /// none). On any non-passive event — a rotation just fired, `fresh`
    /// (refresh), or a non-zero `advance` (next/previous) — and when `new`
    /// is non-empty, the cursor is advanced further until `resolve_index`
    /// lands on one of the new indices, so the snap persists across
    /// subsequent requests rather than being a one-shot override. Passive
    /// polling (plain GET, no rotation) leaves the cursor where it is.
    pub fn pick_index(
        &mut self,
        now: DateTime<Utc>,
        advance: i64,
        fresh: bool,
        new: &[usize],
        n: usize,
    ) -> usize {
        let rotated = self.maybe_rotate(now);
        self.advance(advance);
        let snap = (rotated || fresh || advance != 0) && !new.is_empty();
        if snap {
            let new_set: HashSet<usize> = new.iter().copied().collect();
            for offset in 0..(n as i64) {
                let idx = resolve_index(self.seed, self.cursor.wrapping_add(offset), n);
                if new_set.contains(&idx) {
                    self.advance(offset);
                    break;
                }
            }
        }
        resolve_index(self.seed, self.cursor, n)
    }

    /// Rotate iff `now` has reached the cached `next_rotation` moment.
    /// After construction, `next_rotation = Some(MIN_UTC)` so the first
    /// call always fires (initialising the placeholder seed). After
    /// rotation the cache is recomputed from `now` — this is the only
    /// safe moment to call `next_after` for `cron-lingo`, whose iterator
    /// can only consult the current system clock.
    fn maybe_rotate(&mut self, now: DateTime<Utc>) -> bool {
        let Some(next) = self.next_rotation else {
            return false;
        };
        if now < next {
            return false;
        }
        let old_seed = self.seed;
        self.seed = rand::rng().random();
        self.cursor = 0;
        self.next_rotation = self
            .rotate
            .as_ref()
            .and_then(|r| r.next_after(now, &self.tz));
        tracing::info!(old_seed, new_seed = self.seed, next = ?self.next_rotation, "rotated screen");
        true
    }
}

/// Fisher-Yates permutation of `[0..n)` seeded by `seed`, indexed by
/// `cursor.rem_euclid(n)`. Panics if `n == 0`.
fn resolve_index(seed: u64, cursor: i64, n: usize) -> usize {
    assert!(n > 0, "resolve_index called with empty album");
    let mut perm: Vec<usize> = (0..n).collect();
    let mut rng = StdRng::seed_from_u64(seed);
    perm.shuffle(&mut rng);
    perm[cursor.rem_euclid(n as i64) as usize]
}

/// Seconds from `now` to `target`, rounded up. `target` in the past
/// returns 0 — a `Refresh: 0` header asks the device to reload at once.
pub fn seconds_until(target: DateTime<Utc>, now: DateTime<Utc>) -> u64 {
    let td = target - now;
    let secs = td.num_seconds().max(0) as u64;
    secs.saturating_add((td.subsec_nanos() > 0) as u64)
}

/// The absolute moment at which an error response should ask the device to
/// retry. Base is `now + error_refresh`, but never later than the device's
/// normal next-fetch target (`next_rotation + wake_delay`) — pushing past
/// that would have the device skip a scheduled rotation. With no rotation
/// schedule the cap doesn't apply.
pub fn calculate_error_refresh_time(
    error_refresh: Duration,
    wake_delay: Duration,
    next_rotation: Option<DateTime<Utc>>,
    now: DateTime<Utc>,
) -> DateTime<Utc> {
    let base = now + error_refresh;
    match next_rotation {
        Some(n) => base.min(n + wake_delay),
        None => base,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;
    use std::collections::HashSet;

    fn tz() -> Tz {
        "Europe/Amsterdam".parse().unwrap()
    }

    fn config(rotate_toml: &str) -> ScreenConfig {
        toml::from_str(&format!(
            r#"
            name = "x"
            width = 800
            height = 480
            share_url = "https://example.com"
            timezone = "Europe/Amsterdam"
            {rotate_toml}
            "#
        ))
        .unwrap()
    }

    #[test]
    fn resolve_index_is_a_permutation() {
        let seen: HashSet<usize> = (0..10).map(|c| resolve_index(42, c, 10)).collect();
        assert_eq!(seen.len(), 10);
    }

    #[test]
    fn resolve_index_wraps_negative_cursor() {
        assert_eq!(resolve_index(42, -1, 5), resolve_index(42, 4, 5));
    }

    #[test]
    fn next_scheduled_rotation_returns_cached_future_trigger() {
        let cfg = config(r#"rotate.cron = "0 0 2 * * *""#);
        let tz = tz();
        let start = tz
            .with_ymd_and_hms(2026, 4, 20, 12, 0, 0)
            .unwrap()
            .with_timezone(&Utc);
        let mut s = ScreenState::new(&cfg);
        s.maybe_rotate(start); // populates next_rotation = next 02:00 after start
        let expected = tz
            .with_ymd_and_hms(2026, 4, 21, 2, 0, 0)
            .unwrap()
            .with_timezone(&Utc);
        assert_eq!(s.next_scheduled_rotation(start), Some(expected));
    }

    #[test]
    fn next_scheduled_rotation_returns_now_when_uninitialised() {
        // Pre-maybe_rotate state holds the ASAP sentinel; `now` is in the
        // past relative to MIN_UTC's "future", so the call clamps to now.
        let cfg = config(r#"rotate.cron = "0 0 2 * * *""#);
        let s = ScreenState::new(&cfg);
        let now = Utc::now();
        assert_eq!(s.next_scheduled_rotation(now), Some(now));
    }

    #[test]
    fn next_scheduled_rotation_none_after_first_rotate_without_schedule() {
        // No schedule: first rotate consumes the ASAP sentinel and writes
        // None (no future trigger), so subsequent calls return None.
        let cfg = config("");
        let mut s = ScreenState::new(&cfg);
        s.maybe_rotate(Utc::now());
        assert!(s.next_scheduled_rotation(Utc::now()).is_none());
    }

    #[test]
    fn first_call_always_rotates() {
        let cfg = config(r#"rotate.cron = "0 0 2 * * *""#);
        let mut s = ScreenState::new(&cfg);
        assert_eq!(s.seed(), 0);
        assert_eq!(s.next_rotation, Some(DateTime::<Utc>::MIN_UTC));
        let rotated = s.maybe_rotate(Utc::now());
        assert!(rotated);
        assert_ne!(s.seed(), 0);
        assert!(s.next_rotation.is_some_and(|t| t > Utc::now()));
    }

    #[test]
    fn first_call_rotates_even_without_schedule() {
        let cfg = config("");
        let mut s = ScreenState::new(&cfg);
        let rotated = s.maybe_rotate(Utc::now());
        assert!(rotated);
        // No schedule, so next_rotation collapses to None after the
        // initial rotation consumes the ASAP sentinel.
        assert_eq!(s.next_rotation, None);
    }

    #[test]
    fn cron_rotate_fires_after_next() {
        let cfg = config(r#"rotate.cron = "0 0 2 * * *""#);
        let tz = tz();
        // Initialise at 20 Apr 12:00 local (first call always rotates).
        let start = tz
            .with_ymd_and_hms(2026, 4, 20, 12, 0, 0)
            .unwrap()
            .with_timezone(&Utc);
        let mut s = ScreenState::new(&cfg);
        s.maybe_rotate(start);
        let seed_before = s.seed();
        s.advance(3);
        // Advance past 02:00 next day — should fire another rotation.
        let later = tz
            .with_ymd_and_hms(2026, 4, 21, 3, 0, 0)
            .unwrap()
            .with_timezone(&Utc);
        let rotated = s.maybe_rotate(later);
        assert!(rotated);
        assert_eq!(s.cursor(), 0);
        assert_ne!(s.seed(), seed_before);
        assert!(s.next_rotation.is_some_and(|t| t > later));
    }

    #[test]
    fn cron_rotate_noop_before_next() {
        let cfg = config(r#"rotate.cron = "0 0 2 * * *""#);
        let tz = tz();
        let start = tz
            .with_ymd_and_hms(2026, 4, 21, 3, 0, 0)
            .unwrap()
            .with_timezone(&Utc);
        let mut s = ScreenState::new(&cfg);
        s.maybe_rotate(start); // initialise; next trigger is 22 Apr 02:00
        s.advance(5);
        let snap = (s.seed(), s.cursor(), s.next_rotation);
        // 10 h later is still 21 Apr 13:00, before the 22 Apr 02:00 trigger.
        let now = start + chrono::Duration::hours(10);
        let rotated = s.maybe_rotate(now);
        assert!(!rotated);
        assert_eq!((s.seed(), s.cursor(), s.next_rotation), snap);
    }

    #[test]
    fn no_schedule_means_no_rotation_after_init() {
        let cfg = config("");
        let mut s = ScreenState::new(&cfg);
        s.maybe_rotate(Utc::now()); // initial rotation
        s.advance(7);
        let snap = (s.seed(), s.cursor(), s.next_rotation);
        let rotated = s.maybe_rotate(Utc::now() + chrono::Duration::days(365));
        assert!(!rotated);
        assert_eq!((s.seed(), s.cursor(), s.next_rotation), snap);
    }

    #[test]
    fn seconds_until_rounds_up() {
        let now = Utc::now();
        assert_eq!(seconds_until(now, now), 0);
        assert_eq!(
            seconds_until(now + chrono::Duration::milliseconds(1), now),
            1
        );
        assert_eq!(
            seconds_until(now + chrono::Duration::milliseconds(1000), now),
            1
        );
        assert_eq!(
            seconds_until(now + chrono::Duration::milliseconds(1001), now),
            2
        );
        assert_eq!(seconds_until(now - chrono::Duration::seconds(5), now), 0);
    }

    #[test]
    fn calculate_error_refresh_time_no_schedule_uses_base() {
        let now = Utc::now();
        let t = calculate_error_refresh_time(Duration::hours(1), Duration::zero(), None, now);
        assert_eq!(t, now + Duration::hours(1));
    }

    #[test]
    fn calculate_error_refresh_time_clamps_to_wake_target_when_sooner() {
        let now = Utc::now();
        // Next rotation in 10 min, wake_delay 5 min → cap is 15 min.
        let next_rotation = now + Duration::minutes(10);
        let t = calculate_error_refresh_time(
            Duration::hours(1),
            Duration::minutes(5),
            Some(next_rotation),
            now,
        );
        assert_eq!(t, next_rotation + Duration::minutes(5));
    }

    #[test]
    fn calculate_error_refresh_time_uses_base_when_wake_target_is_later() {
        let now = Utc::now();
        // Next rotation in 6 h → 1 h error_refresh wins.
        let next_rotation = now + Duration::hours(6);
        let t = calculate_error_refresh_time(
            Duration::hours(1),
            Duration::zero(),
            Some(next_rotation),
            now,
        );
        assert_eq!(t, now + Duration::hours(1));
    }
}
