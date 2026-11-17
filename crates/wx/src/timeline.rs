//! What to show at a chosen time, the way the Aviation Weather Center's GFA does it.
//!
//! Every layer is synced to one time. Radar and satellite are observations, so they exist only up
//! to the present; G-AIRMET hazards are forecasts, valid at instants three hours apart.
//!
//! * radar: a loop of up to [`LOOP_FRAMES`] frames, [`LOOP_STEP`] seconds apart, ending at the
//!   chosen time (GFA: the five images ending at the slider's time, 40 minutes)
//! * satellite: the newest image at or before the chosen time, if it is recent enough
//! * hazards: the snapshot valid nearest the chosen time

/// Frames in a radar loop: 5 frames, [`LOOP_STEP`] apart, span 40 minutes.
pub const LOOP_FRAMES: usize = 5;
/// The spacing between a radar loop's frames, seconds.
pub const LOOP_STEP: i64 = 600;
/// A frame stands for a loop slot only if it is this close to the slot's time (seconds).
pub const RADAR_SLACK: i64 = 240;
/// A satellite image older than this, relative to the chosen time, is not shown (seconds).
pub const SAT_MAX_AGE: i64 = 30 * 60;
/// A hazard snapshot further than this from the chosen time is not shown (seconds).
///
/// Snapshots are 3 hours apart, so wherever forecasts exist one is always within reach; past their
/// range, none is better than the nearest one from many hours away.
pub const HAZARD_REACH: i64 = 90 * 60;

/// What each layer shows at one chosen time; every time is Unix seconds.
#[derive(Debug, Clone, PartialEq)]
pub struct Selection {
    /// radar frame times, oldest first, each the frame nearest its slot; empty past the present
    pub radar: Vec<i64>,
    /// The satellite image's time, or `None` when none is recent enough.
    pub satellite: Option<i64>,
    /// The valid time of the hazard snapshot to show, or `None` when none is within reach.
    pub hazards: Option<i64>,
}

/// The frame time nearest `want` within `slack`, from `times` (sorted ascending), never later
/// than `limit`.
fn nearest(times: &[i64], want: i64, slack: i64, limit: i64) -> Option<i64> {
    let i = times.partition_point(|&t| t <= want);
    let candidates = [i.checked_sub(1).map(|k| times[k]), times.get(i).copied()];
    candidates
        .into_iter()
        .flatten()
        .filter(|&t| t <= limit && (t - want).abs() <= slack)
        .min_by_key(|&t| ((t - want).abs(), -t))
}

/// Choose what to show at `at`. The lists are what exists (sorted ascending): radar frames on the
/// server, satellite images, and hazard snapshot valid times.
pub fn select(at: i64, radar: &[i64], satellite: &[i64], hazards: &[i64]) -> Selection {
    let loop_times: Vec<i64> = (0..LOOP_FRAMES as i64)
        .rev()
        .filter_map(|k| nearest(radar, at - k * LOOP_STEP, RADAR_SLACK, at))
        .collect();
    // no frame can fill two slots: they are LOOP_STEP apart and a frame must be within
    // RADAR_SLACK of its slot, which is less than half of that
    const _: () = assert!(2 * RADAR_SLACK < LOOP_STEP);
    let sat_i = satellite.partition_point(|&t| t <= at);
    let sat = sat_i
        .checked_sub(1)
        .map(|k| satellite[k])
        .filter(|&t| at - t <= SAT_MAX_AGE);
    let hz = hazards
        .iter()
        .copied()
        .filter(|&t| (t - at).abs() <= HAZARD_REACH)
        .min_by_key(|&t| ((t - at).abs(), t));
    Selection {
        radar: loop_times,
        satellite: sat,
        hazards: hz,
    }
}

/// Snap a chosen time to the bar's step (seconds), towards the past.
pub fn snap(t: i64, step: i64) -> i64 {
    t.div_euclid(step) * step
}

#[cfg(test)]
mod tests {
    use super::*;

    /// MRMS-like: a frame every 2 minutes (at :00:30 past), for `hours` hours ending at `end`.
    fn frames(end: i64, hours: i64) -> Vec<i64> {
        (0..hours * 30).rev().map(|k| end - k * 120).collect()
    }

    const NOW: i64 = 1_794_870_000;

    #[test]
    fn the_loop_ends_at_the_chosen_time_ten_minutes_apart() {
        let radar = frames(NOW, 24);
        let s = select(NOW - 3 * 3600, &radar, &[], &[]);
        assert_eq!(s.radar.len(), LOOP_FRAMES);
        let want: Vec<i64> = (0..5).rev().map(|k| NOW - 3 * 3600 - k * 600).collect();
        assert_eq!(s.radar, want, "frames exist exactly at each slot");
        assert!(s.radar.windows(2).all(|w| w[0] < w[1]));
    }

    #[test]
    fn nothing_from_after_the_chosen_time() {
        // frames at odd times: the slot at `at` must not take a frame from after `at`
        let radar = vec![NOW - 1260, NOW - 610, NOW + 60];
        let s = select(NOW, &radar, &[], &[]);
        assert!(s.radar.iter().all(|&t| t <= NOW), "{:?}", s.radar);
        assert_eq!(s.radar, vec![NOW - 1260, NOW - 610]);
    }

    #[test]
    fn a_gap_in_the_archive_leaves_slots_empty_rather_than_repeating_a_frame() {
        // only one frame in the last 40 minutes: a loop of one, not five copies
        let radar = vec![NOW - 5000, NOW - 100];
        let s = select(NOW, &radar, &[], &[]);
        assert_eq!(s.radar, vec![NOW - 100]);
        // nothing within slack of any slot: no radar
        assert_eq!(
            select(NOW, &[NOW - 4000], &[], &[]).radar,
            Vec::<i64>::new()
        );
    }

    #[test]
    fn the_future_has_no_observations_but_has_forecasts() {
        let radar = frames(NOW, 24);
        let sat: Vec<i64> = (0..78).rev().map(|k| NOW - k * 300).collect();
        let hz = vec![NOW - 3600, NOW + 2 * 3600, NOW + 5 * 3600, NOW + 8 * 3600];
        let s = select(NOW + 6 * 3600, &radar, &sat, &hz);
        assert_eq!(s.radar, Vec::<i64>::new());
        assert_eq!(s.satellite, None);
        assert_eq!(s.hazards, Some(NOW + 5 * 3600));
    }

    #[test]
    fn hazards_only_from_a_snapshot_near_the_chosen_time() {
        let hz = vec![0, 10_800];
        assert_eq!(
            select(10_800 + HAZARD_REACH, &[], &[], &hz).hazards,
            Some(10_800)
        );
        assert_eq!(
            select(10_800 + HAZARD_REACH + 1, &[], &[], &hz).hazards,
            None,
            "17 hours away is not the hazard picture then"
        );
        assert_eq!(select(-HAZARD_REACH - 1, &[], &[], &hz).hazards, None);
    }

    #[test]
    fn satellite_is_the_newest_image_not_after_the_time_and_not_stale() {
        let sat = vec![NOW - 900, NOW - 600, NOW - 300];
        assert_eq!(select(NOW - 400, &[], &sat, &[]).satellite, Some(NOW - 600));
        assert_eq!(select(NOW, &[], &sat, &[]).satellite, Some(NOW - 300));
        // before its history begins: none (not the oldest image passed off as then)
        assert_eq!(select(NOW - 3 * 3600, &[], &sat, &[]).satellite, None);
        // a feed that stopped: older than 30 minutes is not shown as current
        assert_eq!(select(NOW + 3600, &[], &sat, &[]).satellite, None);
    }

    #[test]
    fn hazards_nearest_on_either_side_earlier_on_a_tie() {
        let hz = vec![0, 10_800, 21_600];
        assert_eq!(select(4000, &[], &[], &hz).hazards, Some(0));
        assert_eq!(select(7000, &[], &[], &hz).hazards, Some(10_800));
        assert_eq!(select(5400, &[], &[], &hz).hazards, Some(0));
        assert_eq!(select(0, &[], &[], &[]).hazards, None);
    }

    #[test]
    fn snapping_goes_to_the_past() {
        assert_eq!(snap(1_000_599, 600), 1_000_200);
        assert_eq!(snap(1_000_200, 600), 1_000_200);
        assert_eq!(snap(-1, 600), -600);
    }
}
