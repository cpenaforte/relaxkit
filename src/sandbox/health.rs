//! The health of relaxkit's OpenCode sandbox plugin (spec 10 item 25). The plugin pins one sandbox runtime exactly.
//! `status` shows how far that pin lags behind npm and how old relaxkit's newest release is. `doctor` warns only when no
//! release has come out for 120 days: a runtime lag alone never warns. Every read fails soft: an unreachable registry or
//! GitHub shows as unknown and never stops setup.

use anyhow::{Context, Result, anyhow};
use semver::Version;
use serde_json::Value;

use super::selftest::{RUNTIME_PACKAGE, RUNTIME_PIN};
use crate::ctx::Ctx;
use crate::net::Net;
use crate::selfupdate::RELEASE_REPO;

/// Days without a relaxkit release before `doctor` warns (spec 10 item 25).
pub const STALE_AFTER_DAYS: u32 = 120;

/// The bundled runtime against the newest runtime npm publishes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimeLag {
    pub bundled: String,
    pub newest: String,
    pub newest_released_days: u32,
    /// Published runtime versions newer than `bundled`.
    pub lag: u32,
}

/// The plugin's health. Each part may be unknown on its own.
pub struct PluginHealth {
    pub runtime: Result<RuntimeLag>,
    /// Days since relaxkit's newest release, which is the release that carries the plugin.
    pub release_age_days: Result<u32>,
}

/// The plugin's health now, read through `ctx.net`.
pub fn plugin_health(ctx: &Ctx) -> PluginHealth {
    health_at(ctx.net.as_ref(), RUNTIME_PIN, now_seconds())
}

/// `plugin_health` for a bundled runtime version and a clock, so tests can fix both.
pub fn health_at(net: &dyn Net, bundled: &str, now: i64) -> PluginHealth {
    PluginHealth {
        runtime: runtime_lag(net, bundled, now),
        release_age_days: release_age_days(net, now),
    }
}

/// Whether no relaxkit release has come out for `STALE_AFTER_DAYS`. An unknown age is never stale, and a runtime lag
/// is not considered at all.
pub fn is_stale(health: &PluginHealth) -> bool {
    matches!(health.release_age_days, Ok(days) if days >= STALE_AFTER_DAYS)
}

/// The health as one line for `status` and `doctor`.
pub fn health_line(health: &PluginHealth) -> String {
    let runtime = match &health.runtime {
        Ok(lag) if lag.lag == 0 => format!("sandbox-runtime {} bundled, up to date", lag.bundled),
        Ok(lag) => format!(
            "sandbox-runtime {} bundled, {} release{} behind (npm's newest is {}, released {} days ago)",
            lag.bundled,
            lag.lag,
            if lag.lag == 1 { "" } else { "s" },
            lag.newest,
            lag.newest_released_days
        ),
        Err(err) => format!("sandbox-runtime health unknown ({err:#})"),
    };
    let release = match &health.release_age_days {
        Ok(days) => format!("relaxkit's newest release is {days} days old"),
        Err(err) => format!("relaxkit's release age is unknown ({err:#})"),
    };
    format!("{runtime}; {release}")
}

fn runtime_lag(net: &dyn Net, bundled: &str, now: i64) -> Result<RuntimeLag> {
    let runtime = npm_document(net, RUNTIME_PACKAGE)?;
    let pinned = Version::parse(bundled)
        .with_context(|| format!("the bundled runtime {bundled} is not a version"))?;
    let mut published: Vec<Version> = runtime["versions"]
        .as_object()
        .ok_or_else(|| anyhow!("{RUNTIME_PACKAGE} lists no versions"))?
        .keys()
        .filter_map(|text| Version::parse(text).ok())
        .filter(|version| version.pre.is_empty())
        .collect();
    published.sort();
    let newest = published
        .last()
        .ok_or_else(|| anyhow!("{RUNTIME_PACKAGE} has no published versions"))?;
    let released_at = |version: &Version| -> Result<i64> {
        let text = runtime["time"][version.to_string()]
            .as_str()
            .ok_or_else(|| anyhow!("{RUNTIME_PACKAGE}@{version} has no release time"))?;
        parse_utc(text)
    };
    Ok(RuntimeLag {
        bundled: bundled.to_string(),
        newest: newest.to_string(),
        newest_released_days: age_days(released_at(newest)?, now),
        lag: u32::try_from(
            published
                .iter()
                .filter(|version| **version > pinned)
                .count(),
        )
        .unwrap_or(u32::MAX),
    })
}

/// Days since relaxkit's newest release, from GitHub's `published_at` (RFC 3339 UTC).
fn release_age_days(net: &dyn Net, now: i64) -> Result<u32> {
    let body = net
        .get_text(&format!(
            "https://api.github.com/repos/{RELEASE_REPO}/releases/latest"
        ))
        .context("cannot read relaxkit's latest release")?;
    let value: Value =
        serde_json::from_str(&body).context("GitHub release response is not JSON")?;
    let published = value
        .get("published_at")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("GitHub release response has no published_at"))?;
    Ok(age_days(parse_utc(published)?, now))
}

/// A package's registry document, read through the net abstraction. Scoped names are encoded as `@scope%2fname`.
fn npm_document(net: &dyn Net, package: &str) -> Result<Value> {
    let encoded = package.replace('/', "%2f");
    let body = net
        .get_text(&format!("https://registry.npmjs.org/{encoded}"))
        .with_context(|| format!("cannot read the npm registry entry for {package}"))?;
    serde_json::from_str(&body)
        .with_context(|| format!("the npm registry entry for {package} is not JSON"))
}

/// Seconds since the epoch of an RFC 3339 UTC time as the registries write it (`2026-11-18T12:00:00.000Z`).
fn parse_utc(text: &str) -> Result<i64> {
    let number = |range: std::ops::Range<usize>| -> Result<i64> {
        text.get(range)
            .and_then(|digits| digits.parse().ok())
            .ok_or_else(|| anyhow!("not an RFC 3339 UTC time: {text}"))
    };
    let days = days_from_civil(number(0..4)?, number(5..7)?, number(8..10)?);
    Ok(days * 86_400 + number(11..13)? * 3_600 + number(14..16)? * 60 + number(17..19)?)
}

/// Days since 1970-01-01 of a proleptic Gregorian date (the standard civil-calendar algorithm).
fn days_from_civil(year: i64, month: i64, day: i64) -> i64 {
    let year = if month <= 2 { year - 1 } else { year };
    let era = if year >= 0 { year } else { year - 399 } / 400;
    let year_of_era = year - era * 400;
    let month_from_march = (month + 9) % 12;
    let day_of_year = (153 * month_from_march + 2) / 5 + day - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    era * 146_097 + day_of_era - 719_468
}

fn now_seconds() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs() as i64)
        .unwrap_or(0)
}

/// Whole days from `released_at` to `now`, never negative.
fn age_days(released_at: i64, now: i64) -> u32 {
    u32::try_from((now - released_at).max(0) / 86_400).unwrap_or(u32::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::net::FakeNet;

    /// 2026-11-22T12:00:00Z.
    const NOW: i64 = 1_795_348_800;
    const RUNTIME_URL: &str = "https://registry.npmjs.org/@anthropic-ai%2fsandbox-runtime";
    const RELEASE_URL: &str = "https://api.github.com/repos/cpenaforte/relaxkit/releases/latest";

    /// The runtime versions the registry serves, with their release times.
    fn runtime_times() -> Vec<(&'static str, &'static str)> {
        vec![
            ("0.0.76", "2026-09-01T00:00:00.000Z"),
            ("0.0.77", "2026-09-10T00:00:00.000Z"),
            ("0.0.78", "2026-09-20T00:00:00.000Z"),
            ("0.0.79", "2026-10-01T00:00:00.000Z"),
            ("0.0.80", "2026-10-10T00:00:00.000Z"),
            ("0.0.81", "2026-11-01T00:00:00.000Z"),
            ("0.0.82", "2026-11-15T00:00:00.000Z"),
            ("0.0.83", "2026-11-19T00:00:00.000Z"),
        ]
    }

    /// A fake network: the runtime's registry entry with `times`, and relaxkit's latest release at `release`, when given.
    fn network(times: &[(&str, &str)], release: Option<&str>) -> FakeNet {
        let versions = times
            .iter()
            .map(|(version, _)| format!("\"{version}\": {{}}"))
            .collect::<Vec<_>>()
            .join(", ");
        let time = times
            .iter()
            .map(|(version, at)| format!("\"{version}\": \"{at}\""))
            .collect::<Vec<_>>()
            .join(", ");
        let body = format!(r#"{{"versions": {{{versions}}}, "time": {{{time}}}}}"#);
        let net = FakeNet::new().with(RUNTIME_URL, &body);
        match release {
            Some(at) => net.with(RELEASE_URL, &format!(r#"{{"published_at": "{at}"}}"#)),
            None => net,
        }
    }

    #[test]
    fn a_pin_on_the_newest_runtime_is_up_to_date() {
        let health = health_at(&network(&runtime_times(), None), "0.0.83", NOW);

        let lag = health.runtime.as_ref().unwrap();
        assert_eq!(lag.lag, 0);
        assert_eq!(lag.newest, "0.0.83");
        assert_eq!(lag.newest_released_days, 3);
        assert!(health_line(&health).starts_with("sandbox-runtime 0.0.83 bundled, up to date;"));
    }

    #[test]
    fn lag_counts_the_newer_runtime_releases() {
        let health = health_at(&network(&runtime_times(), None), "0.0.80", NOW);

        let lag = health.runtime.as_ref().unwrap();
        assert_eq!(lag.lag, 3);
        assert_eq!(lag.newest_released_days, 3);
    }

    #[test]
    fn prereleases_are_not_counted() {
        let mut times = runtime_times();
        times.push(("0.0.84-beta.1", "2026-11-21T00:00:00.000Z"));
        let health = health_at(&network(&times, None), "0.0.83", NOW);

        let lag = health.runtime.as_ref().unwrap();
        assert_eq!(lag.newest, "0.0.83");
        assert_eq!(lag.lag, 0);
    }

    #[test]
    fn the_release_age_is_days_since_relaxkits_newest_release() {
        let health = health_at(
            &network(&runtime_times(), Some("2026-11-12T00:00:00Z")),
            "0.0.83",
            NOW,
        );

        assert_eq!(*health.release_age_days.as_ref().unwrap(), 10);
    }

    #[test]
    fn only_120_days_without_a_release_is_stale() {
        let stale = |released: &str| {
            is_stale(&health_at(
                &network(&runtime_times(), Some(released)),
                "0.0.83",
                NOW,
            ))
        };

        assert!(!stale("2026-07-26T00:00:00Z"), "119 days is not stale");
        assert!(stale("2026-07-25T00:00:00Z"), "120 days is stale");
        assert!(stale("2026-07-24T00:00:00Z"), "121 days is stale");
    }

    #[test]
    fn a_runtime_lag_alone_is_never_stale() {
        // The pin is 0.0.76, seven releases behind, and relaxkit released three days ago.
        let health = health_at(
            &network(&runtime_times(), Some("2026-11-19T00:00:00Z")),
            "0.0.76",
            NOW,
        );

        assert_eq!(health.runtime.as_ref().unwrap().lag, 7);
        assert!(!is_stale(&health));
    }

    #[test]
    fn unreachable_sources_are_unknown_and_never_stale() {
        let health = health_at(&FakeNet::new(), "0.0.83", NOW);

        assert!(health.runtime.is_err());
        assert!(health.release_age_days.is_err());
        assert!(!is_stale(&health));
        let line = health_line(&health);
        assert!(line.contains("health unknown"), "{line}");
        assert!(line.contains("release age is unknown"), "{line}");
    }

    #[test]
    fn the_line_names_the_lag_and_the_release_age() {
        let health = health_at(
            &network(&runtime_times(), Some("2026-11-12T00:00:00Z")),
            "0.0.80",
            NOW,
        );

        assert_eq!(
            health_line(&health),
            "sandbox-runtime 0.0.80 bundled, 3 releases behind (npm's newest is 0.0.83, released 3 days ago); relaxkit's newest release is 10 days old"
        );
    }
}
