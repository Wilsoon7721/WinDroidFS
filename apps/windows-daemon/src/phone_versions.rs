//! Which phone app versions this PC app works with

/// One release of the phone app
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Release {
    /// versionCode
    pub code: u32,
    /// versionName for display
    pub name: &'static str,
    /// This app doesn't work with phone apps older than this
    pub breaking: bool,
}

/// Every phone app release, oldest first
pub const RELEASES: &[Release] = &[
    Release {
        code: 1,
        name: "1.0.0",
        breaking: false,
    },
    Release {
        code: 2,
        name: "1.1.0",
        breaking: false,
    },
];

/// The first phone app that shows its own "Outdated version" screen when told to
pub const SHOWS_UPDATE_SCREEN: u32 = 2;

/// What this PC app makes of a phone app.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Verdict {
    /// The newest one this PC app knows
    Current,
    /// Works, but `latest` is out
    Outdated { latest: Release },
    /// Won't work until it's at least `needs`. `latest` is the one to get.
    Unsupported { needs: Release, latest: Release },
    /// Newer than any this PC app knows, so the PC app is probably the one out of date.
    Newer,
}

/// The verdict on the phone app with version code `code`. 0 is what apps from before codes were sent report: 1.0.0
pub fn judge(code: u32) -> Verdict {
    judge_in(RELEASES, code)
}

fn judge_in(releases: &[Release], code: u32) -> Verdict {
    let code = code.max(1);
    let latest = *releases
        .last()
        .expect("RELEASES lists at least one release");
    if code > latest.code {
        return Verdict::Newer;
    }
    if let Some(needs) = releases.iter().rev().find(|r| r.breaking)
        && code < needs.code
    {
        return Verdict::Unsupported {
            needs: *needs,
            latest,
        };
    }
    if code < latest.code {
        Verdict::Outdated { latest }
    } else {
        Verdict::Current
    }
}

/// The name of the phone app with version code `code`, e.g. "1.0.0", or the code itself if this PC app doesn't know it
pub fn name_of(code: u32, reported: &str) -> String {
    if !reported.is_empty() {
        return reported.to_string();
    }
    let code = code.max(1);
    RELEASES
        .iter()
        .find(|r| r.code == code)
        .map_or_else(|| format!("version {code}"), |r| r.name.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    const fn release(code: u32, breaking: bool) -> Release {
        Release {
            code,
            name: "x",
            breaking,
        }
    }

    #[test]
    fn the_newest_is_current_and_older_ones_outdated() {
        let releases = [release(1, false), release(2, false), release(3, false)];
        assert_eq!(judge_in(&releases, 3), Verdict::Current);
        assert_eq!(
            judge_in(&releases, 2),
            Verdict::Outdated {
                latest: releases[2]
            }
        );
        // Before codes were sent is 1.0.0.
        assert_eq!(judge_in(&releases, 0), judge_in(&releases, 1));
        assert_eq!(judge_in(&releases, 4), Verdict::Newer);
    }

    #[test]
    fn anything_before_a_breaking_release_is_unsupported() {
        let releases = [
            release(1, false),
            release(2, true),
            release(3, false),
            release(4, true),
            release(5, false),
        ];
        let latest = releases[4];
        for code in 1..=3 {
            assert_eq!(
                judge_in(&releases, code),
                Verdict::Unsupported {
                    needs: releases[3],
                    latest
                },
                "code {code}"
            );
        }
        assert_eq!(judge_in(&releases, 4), Verdict::Outdated { latest });
        assert_eq!(judge_in(&releases, 5), Verdict::Current);
    }

    #[test]
    fn the_real_list_is_in_order() {
        assert!(RELEASES.windows(2).all(|w| w[0].code < w[1].code));
    }

    #[test]
    fn names_versions_for_people() {
        assert_eq!(name_of(0, ""), "1.0.0");
        assert_eq!(name_of(2, "1.1.0-beta"), "1.1.0-beta");
        assert_eq!(name_of(99, ""), "version 99");
    }
}
