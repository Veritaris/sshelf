//! The update check (SPEC.md §10 *Update check*, M14): asks GitHub for the
//! latest release, compares it with this build, and keeps `updates.toml`
//! (last check, its result, the ETag, the latest release, a skipped
//! version). It never downloads anything.
//!
//! The [`Checker`] is a thread of its own, started by core when the app
//! asks (never in the tour's sandbox): one request about 5 s after start,
//! then every 24 h, 1 h after a failure, at GitHub's reset time after a
//! rate limit, and on *Check now*. The request goes through a
//! [`Transport`] ([`Ureq`] for real; tests inject their own or point it at a
//! local server). Waiting is `recv_timeout` until the next check is due.

use std::io;
use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::storage::{Store, Versioned};

/// GitHub's latest release of sshelf (never a draft or a pre-release).
pub const LATEST_URL: &str = "https://api.github.com/repos/Veritaris/sshelf/releases/latest";
/// What `html_url` must start with: it is opened in the browser.
const RELEASE_PAGES: &str = "https://github.com/Veritaris/sshelf/";
/// The request's time limit.
pub const TIMEOUT: Duration = Duration::from_secs(10);
/// The first check after start.
pub const FIRST: Duration = Duration::from_secs(5);
/// Between successful checks.
pub const DAILY: Duration = Duration::from_secs(24 * 60 * 60);
/// After a failed one.
pub const RETRY: Duration = Duration::from_secs(60 * 60);

/// A release version: `X.Y.Z`, compared part by part.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct Version(pub u64, pub u64, pub u64);

impl Version {
    /// `X.Y.Z` with an optional leading `v`; anything else is `None` (`dev`
    /// among them).
    pub fn parse(text: &str) -> Option<Self> {
        let text = text.trim();
        let text = text.strip_prefix('v').unwrap_or(text);
        let mut parts = text.split('.').map(|part| {
            (!part.is_empty() && part.bytes().all(|b| b.is_ascii_digit()))
                .then(|| part.parse::<u64>().ok())
                .flatten()
        });
        let version = Version(parts.next()??, parts.next()??, parts.next()??);
        parts.next().is_none().then_some(version)
    }
}

impl std::fmt::Display for Version {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}.{}.{}", self.0, self.1, self.2)
    }
}

/// The latest release as GitHub reported it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Release {
    /// `X.Y.Z`, without a leading `v`.
    pub version: String,
    /// Its page on GitHub (*What's new*).
    pub url: String,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "stamp::option"
    )]
    pub published: Option<SystemTime>,
}

/// Why a check failed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "reason", rename_all = "kebab-case")]
pub enum Failure {
    /// No connection to GitHub (no network, name not found, refused).
    Offline,
    /// No answer within [`TIMEOUT`].
    Timeout,
    /// GitHub's rate limit: no request before `until`.
    RateLimited {
        #[serde(with = "stamp")]
        until: SystemTime,
    },
    /// An HTTP status other than 200 / 304.
    Http { status: u16 },
    /// A 200 that isn't a release (no JSON, a tag that isn't `X.Y.Z`, a
    /// page that isn't sshelf's).
    BadAnswer,
    /// Anything else on the way (TLS, a proxy).
    Other { message: String },
}

/// `updates.toml`: written by the checker only.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct UpdatesFile {
    /// The last check, whatever came of it.
    #[serde(skip_serializing_if = "Option::is_none", with = "stamp::option")]
    pub checked_at: Option<SystemTime>,
    /// The last check that got an answer (200 or 304).
    #[serde(skip_serializing_if = "Option::is_none", with = "stamp::option")]
    pub succeeded_at: Option<SystemTime>,
    /// GitHub's ETag for [`UpdatesFile::latest`] (`If-None-Match`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub etag: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub latest: Option<Release>,
    /// *Skip this version*.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub skipped: Option<String>,
    /// The last check's failure; `None` after a success.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub failure: Option<Failure>,
}

impl Versioned for UpdatesFile {
    const FILE: &'static str = "updates.toml";
    const VERSION: u32 = 1;
}

/// What the app shows (Settings › Updates, the toast, the rail dot).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UpdateStatus {
    /// This build's version (`dev` outside a release build).
    pub current: String,
    /// The *Check for updates* setting.
    pub enabled: bool,
    /// Checks run on their own: enabled, the first-run note shown, not a
    /// `dev` build.
    pub auto: bool,
    /// A request is on its way.
    pub checking: bool,
    /// This run made a check (automatic or *Check now*).
    pub checked_this_run: bool,
    pub latest: Option<Release>,
    pub skipped: Option<String>,
    pub checked_at: Option<SystemTime>,
    pub succeeded_at: Option<SystemTime>,
    pub failure: Option<Failure>,
    /// When the next automatic check runs.
    pub next_check: Option<SystemTime>,
}

impl UpdateStatus {
    /// A build without a release version (`dev`): nothing is compared.
    pub fn dev(&self) -> bool {
        Version::parse(&self.current).is_none()
    }

    /// The latest release, if it is newer than this build.
    pub fn newer(&self) -> Option<&Release> {
        let current = Version::parse(&self.current)?;
        self.latest
            .as_ref()
            .filter(|r| Version::parse(&r.version).is_some_and(|v| v > current))
    }

    /// This build is newer than the latest release (a build from `main`
    /// with a release number ahead of GitHub's).
    pub fn ahead(&self) -> bool {
        let latest = self
            .latest
            .as_ref()
            .and_then(|r| Version::parse(&r.version));
        matches!((Version::parse(&self.current), latest), (Some(c), Some(l)) if c > l)
    }

    /// What the toast and the rail dot announce: a newer release that isn't
    /// skipped, while checks are on or after a check this run.
    pub fn announced(&self) -> Option<&Release> {
        self.newer()
            .filter(|r| self.skipped.as_deref() != Some(r.version.as_str()))
            .filter(|_| self.enabled || self.checked_this_run)
    }
}

/// When the next automatic check is due; `None` while they are off.
/// `started`: when the checker started; `last`: this run's last check.
pub fn next_check(
    auto: bool,
    started: SystemTime,
    first: Duration,
    last: Option<SystemTime>,
    failure: Option<&Failure>,
) -> Option<SystemTime> {
    if !auto {
        return None;
    }
    let earliest = started + first;
    if let Some(Failure::RateLimited { until }) = failure {
        return Some((*until).max(earliest));
    }
    Some(match last {
        None => earliest,
        Some(at) if failure.is_some() => at + RETRY,
        Some(at) => at + DAILY,
    })
}

/// The request the checker makes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HttpRequest {
    pub url: String,
    /// `sshelf/<version>`.
    pub user_agent: String,
    /// `If-None-Match`.
    pub etag: Option<String>,
}

/// What came back, as far as the check reads it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct HttpResponse {
    pub status: u16,
    pub etag: Option<String>,
    /// `x-ratelimit-reset`: Unix seconds.
    pub ratelimit_reset: Option<u64>,
    /// `retry-after`: seconds.
    pub retry_after: Option<u64>,
    pub body: String,
}

/// One GET. Errors are already sorted into [`Failure`]s.
pub trait Transport: Send {
    fn get(&self, request: &HttpRequest) -> Result<HttpResponse, Failure>;
}

/// The real transport: `ureq` with rustls, [`TIMEOUT`] for the whole
/// request, and the proxy from `HTTPS_PROXY` / `ALL_PROXY` (minus
/// `NO_PROXY`).
pub struct Ureq {
    pub timeout: Duration,
    /// Off for tests against a local server.
    pub env_proxy: bool,
}

impl Default for Ureq {
    fn default() -> Self {
        Self {
            timeout: TIMEOUT,
            env_proxy: true,
        }
    }
}

impl Transport for Ureq {
    fn get(&self, request: &HttpRequest) -> Result<HttpResponse, Failure> {
        let mut config = ureq::Agent::config_builder()
            .timeout_global(Some(self.timeout))
            .http_status_as_error(false)
            .user_agent(request.user_agent.as_str());
        if !self.env_proxy {
            config = config.proxy(None);
        }
        let agent: ureq::Agent = config.build().into();
        let mut call = agent
            .get(&request.url)
            .header("Accept", "application/vnd.github+json");
        if let Some(etag) = &request.etag {
            call = call.header("If-None-Match", etag);
        }
        let mut response = call.call().map_err(failure)?;
        let header = |name: &str| {
            response
                .headers()
                .get(name)
                .and_then(|v| v.to_str().ok())
                .map(str::to_string)
        };
        let number = |name: &str| header(name).and_then(|v| v.trim().parse().ok());
        let mut answer = HttpResponse {
            status: response.status().as_u16(),
            etag: header("etag"),
            ratelimit_reset: number("x-ratelimit-reset"),
            retry_after: number("retry-after"),
            body: String::new(),
        };
        if answer.status == 200 {
            answer.body = response
                .body_mut()
                .with_config()
                .limit(1024 * 1024)
                .read_to_string()
                .map_err(failure)?;
        }
        Ok(answer)
    }
}

/// `ureq`'s error as a [`Failure`].
fn failure(error: ureq::Error) -> Failure {
    match error {
        ureq::Error::Timeout(_) => Failure::Timeout,
        ureq::Error::HostNotFound | ureq::Error::ConnectionFailed => Failure::Offline,
        ureq::Error::Io(e) if e.kind() == io::ErrorKind::TimedOut => Failure::Timeout,
        ureq::Error::Io(_) => Failure::Offline,
        other => Failure::Other {
            message: other.to_string(),
        },
    }
}

/// What a check found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Answer {
    Release {
        release: Release,
        etag: Option<String>,
    },
    /// `304`: the stored release still is the latest.
    NotModified,
}

/// The release JSON's fields the check reads.
#[derive(Deserialize)]
struct GithubRelease {
    tag_name: String,
    html_url: String,
    #[serde(default)]
    published_at: Option<String>,
}

/// Reads GitHub's answer at `now` (a rate limit's wait is relative to it).
pub fn evaluate(response: &HttpResponse, now: SystemTime) -> Result<Answer, Failure> {
    match response.status {
        200 => {
            let json: GithubRelease =
                serde_json::from_str(&response.body).map_err(|_| Failure::BadAnswer)?;
            let version = Version::parse(&json.tag_name).ok_or(Failure::BadAnswer)?;
            if !json.html_url.starts_with(RELEASE_PAGES) {
                return Err(Failure::BadAnswer);
            }
            Ok(Answer::Release {
                release: Release {
                    version: version.to_string(),
                    url: json.html_url,
                    published: json
                        .published_at
                        .and_then(|t| humantime::parse_rfc3339_weak(&t).ok()),
                },
                etag: response.etag.clone(),
            })
        }
        304 => Ok(Answer::NotModified),
        403 | 429 => {
            let until = response
                .retry_after
                .map(|secs| now + Duration::from_secs(secs))
                .or_else(|| {
                    response
                        .ratelimit_reset
                        .map(|at| UNIX_EPOCH + Duration::from_secs(at))
                });
            match until {
                Some(until) => Err(Failure::RateLimited { until }),
                None => Err(Failure::Http {
                    status: response.status,
                }),
            }
        }
        status => Err(Failure::Http { status }),
    }
}

/// What the checker is told while it runs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Request {
    /// The *Check for updates* setting and whether the first-run note was
    /// shown.
    Configure { enabled: bool, noted: bool },
    /// *Check now*: also while off, and in a `dev` build.
    CheckNow,
    /// *Skip this version*.
    Skip(String),
    /// *Undo* in Settings.
    Unskip,
}

/// How a [`Checker`] is set up.
pub struct Options {
    /// This build's version.
    pub version: String,
    /// The release URL ([`LATEST_URL`]; a local server in tests).
    pub url: String,
    /// Until the first automatic check ([`FIRST`]).
    pub first: Duration,
    pub enabled: bool,
    pub noted: bool,
}

enum Message {
    Request(Request),
    Shutdown,
}

/// The update check's thread; dropping it ends the thread (a request on its
/// way finishes first, unseen).
pub struct Checker {
    tx: Sender<Message>,
}

impl Checker {
    /// Loads `updates.toml` from `app_dir`, reports the status at once, then
    /// checks on schedule.
    pub fn start(
        app_dir: PathBuf,
        options: Options,
        transport: Box<dyn Transport>,
        on_status: impl Fn(UpdateStatus) + Send + 'static,
    ) -> Self {
        let (tx, rx) = mpsc::channel();
        thread::Builder::new()
            .name("sshelf-updates".into())
            .spawn(move || {
                let mut store = Store::new(app_dir);
                let file = store.load::<UpdatesFile>().data;
                Worker {
                    store,
                    file,
                    transport,
                    started: SystemTime::now(),
                    last: None,
                    checking: false,
                    options,
                    on_status: Box::new(on_status),
                }
                .run(rx);
            })
            .expect("spawning the update checker");
        Self { tx }
    }

    pub fn send(&self, request: Request) {
        let _ = self.tx.send(Message::Request(request));
    }
}

impl Drop for Checker {
    fn drop(&mut self) {
        let _ = self.tx.send(Message::Shutdown);
    }
}

struct Worker {
    store: Store,
    file: UpdatesFile,
    transport: Box<dyn Transport>,
    started: SystemTime,
    /// This run's last check.
    last: Option<SystemTime>,
    checking: bool,
    options: Options,
    on_status: Box<dyn Fn(UpdateStatus) + Send>,
}

impl Worker {
    fn auto(&self) -> bool {
        self.options.enabled
            && self.options.noted
            && Version::parse(&self.options.version).is_some()
    }

    fn next(&self) -> Option<SystemTime> {
        next_check(
            self.auto(),
            self.started,
            self.options.first,
            self.last,
            self.file.failure.as_ref(),
        )
    }

    fn status(&self) -> UpdateStatus {
        UpdateStatus {
            current: self.options.version.clone(),
            enabled: self.options.enabled,
            auto: self.auto(),
            checking: self.checking,
            checked_this_run: self.last.is_some(),
            latest: self.file.latest.clone(),
            skipped: self.file.skipped.clone(),
            checked_at: self.file.checked_at,
            succeeded_at: self.file.succeeded_at,
            failure: self.file.failure.clone(),
            next_check: self.next(),
        }
    }

    fn report(&self) {
        (self.on_status)(self.status());
    }

    fn run(mut self, rx: Receiver<Message>) {
        self.report();
        loop {
            let message = match self.next() {
                Some(at) => {
                    let wait = at
                        .duration_since(SystemTime::now())
                        .unwrap_or(Duration::ZERO);
                    match rx.recv_timeout(wait) {
                        Ok(message) => message,
                        Err(RecvTimeoutError::Timeout) => {
                            self.check();
                            continue;
                        }
                        Err(RecvTimeoutError::Disconnected) => return,
                    }
                }
                None => match rx.recv() {
                    Ok(message) => message,
                    Err(_) => return,
                },
            };
            match message {
                Message::Shutdown => return,
                Message::Request(Request::Configure { enabled, noted }) => {
                    self.options.enabled = enabled;
                    self.options.noted = noted;
                    self.report();
                }
                Message::Request(Request::CheckNow) => self.check(),
                Message::Request(Request::Skip(version)) => {
                    self.file.skipped = Some(version);
                    self.save();
                    self.report();
                }
                Message::Request(Request::Unskip) => {
                    self.file.skipped = None;
                    self.save();
                    self.report();
                }
            }
        }
    }

    /// One request, its result kept in `updates.toml`.
    fn check(&mut self) {
        self.checking = true;
        self.report();
        let request = HttpRequest {
            url: self.options.url.clone(),
            user_agent: format!("sshelf/{}", self.options.version),
            // Only with the release it stands for.
            etag: self.file.latest.as_ref().and(self.file.etag.clone()),
        };
        let result = self.transport.get(&request);
        let now = SystemTime::now();
        match result.and_then(|response| evaluate(&response, now)) {
            Ok(Answer::Release { release, etag }) => {
                self.file.latest = Some(release);
                self.file.etag = etag;
                self.file.succeeded_at = Some(now);
                self.file.failure = None;
            }
            Ok(Answer::NotModified) => {
                self.file.succeeded_at = Some(now);
                self.file.failure = None;
            }
            Err(failure) => self.file.failure = Some(failure),
        }
        self.file.checked_at = Some(now);
        self.last = Some(now);
        self.checking = false;
        self.save();
        self.report();
    }

    /// A file that can't be written (read-only, a newer version) keeps the
    /// state in memory for this run.
    fn save(&self) {
        let _ = self.store.save(&self.file);
    }
}

/// `SystemTime` as an RFC 3339 string (seconds, UTC) in `updates.toml`.
mod stamp {
    use std::time::SystemTime;

    use serde::{Deserialize, Deserializer, Serializer};

    pub fn serialize<S: Serializer>(time: &SystemTime, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&humantime::format_rfc3339_seconds(*time).to_string())
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<SystemTime, D::Error> {
        let text = String::deserialize(d)?;
        humantime::parse_rfc3339_weak(&text).map_err(serde::de::Error::custom)
    }

    pub mod option {
        use super::*;

        pub fn serialize<S: Serializer>(
            time: &Option<SystemTime>,
            s: S,
        ) -> Result<S::Ok, S::Error> {
            match time {
                Some(time) => super::serialize(time, s),
                None => s.serialize_none(),
            }
        }

        pub fn deserialize<'de, D: Deserializer<'de>>(
            d: D,
        ) -> Result<Option<SystemTime>, D::Error> {
            super::deserialize(d).map(Some)
        }
    }
}

#[cfg(test)]
mod tests {
    use std::io::{BufRead, BufReader, Write};
    use std::net::TcpListener;
    use std::sync::{Arc, Mutex};

    use super::*;

    /// A made-up release, as GitHub's `/releases/latest` returns it (the
    /// fields the check reads, and a few it ignores).
    const RELEASE_JSON: &str = r#"{
        "url": "https://api.github.com/repos/Veritaris/sshelf/releases/1",
        "html_url": "https://github.com/Veritaris/sshelf/releases/tag/0.4.0",
        "tag_name": "0.4.0",
        "name": "sshelf 0.4.0",
        "draft": false,
        "prerelease": false,
        "published_at": "2026-11-02T10:00:00Z",
        "assets": []
    }"#;

    fn at(secs: u64) -> SystemTime {
        UNIX_EPOCH + Duration::from_secs(1_800_000_000 + secs)
    }

    fn ok(body: &str) -> HttpResponse {
        HttpResponse {
            status: 200,
            etag: Some("\"e1\"".into()),
            body: body.into(),
            ..HttpResponse::default()
        }
    }

    fn status(current: &str, latest: Option<&str>) -> UpdateStatus {
        UpdateStatus {
            current: current.into(),
            enabled: true,
            auto: true,
            checking: false,
            checked_this_run: true,
            latest: latest.map(|v| Release {
                version: v.into(),
                url: format!("{RELEASE_PAGES}releases/tag/{v}"),
                published: None,
            }),
            skipped: None,
            checked_at: None,
            succeeded_at: None,
            failure: None,
            next_check: None,
        }
    }

    #[test]
    fn versions_parse_and_compare_part_by_part() {
        assert_eq!(Version::parse("0.4.0"), Some(Version(0, 4, 0)));
        assert_eq!(Version::parse("v1.10.2"), Some(Version(1, 10, 2)));
        assert!(Version(0, 10, 0) > Version(0, 9, 9));
        for bad in [
            "dev",
            "",
            "1.2",
            "1.2.3.4",
            "1.2.x",
            "1..3",
            "v",
            "1.2.3-rc1",
            "+1.2.3",
        ] {
            assert_eq!(Version::parse(bad), None, "{bad}");
        }
        assert_eq!(Version(1, 2, 3).to_string(), "1.2.3");
    }

    #[test]
    fn only_a_newer_release_is_announced() {
        assert_eq!(
            status("0.3.2", Some("0.4.0")).newer().unwrap().version,
            "0.4.0"
        );
        assert!(status("0.4.0", Some("0.4.0")).newer().is_none());
        let ahead = status("0.4.1", Some("0.4.0"));
        assert!(ahead.newer().is_none() && ahead.ahead());
        // A dev build compares nothing.
        let dev = status("dev", Some("0.4.0"));
        assert!(dev.dev() && dev.newer().is_none() && !dev.ahead());
        // Skipped: known, not announced; the next one is.
        let mut skipped = status("0.3.2", Some("0.4.0"));
        skipped.skipped = Some("0.4.0".into());
        assert!(skipped.newer().is_some() && skipped.announced().is_none());
        skipped.skipped = Some("0.3.9".into());
        assert!(skipped.announced().is_some());
        // Off: a stored release isn't announced unless checked this run.
        let mut off = status("0.3.2", Some("0.4.0"));
        off.enabled = false;
        off.checked_this_run = false;
        assert!(off.announced().is_none());
        off.checked_this_run = true;
        assert!(off.announced().is_some());
    }

    #[test]
    fn a_release_is_read_from_githubs_json() {
        let answer = evaluate(&ok(RELEASE_JSON), at(0)).unwrap();
        let Answer::Release { release, etag } = answer else {
            panic!("{answer:?}");
        };
        assert_eq!(release.version, "0.4.0");
        assert_eq!(
            release.url,
            "https://github.com/Veritaris/sshelf/releases/tag/0.4.0"
        );
        assert_eq!(
            release.published,
            Some(UNIX_EPOCH + Duration::from_secs(1_793_613_600))
        );
        assert_eq!(etag.as_deref(), Some("\"e1\""));
        // A leading v is dropped.
        let v = RELEASE_JSON.replace("\"tag_name\": \"0.4.0\"", "\"tag_name\": \"v0.4.0\"");
        assert!(matches!(
            evaluate(&ok(&v), at(0)),
            Ok(Answer::Release { release, .. }) if release.version == "0.4.0"
        ));
    }

    #[test]
    fn odd_answers_fail_the_check() {
        let bad_tag = RELEASE_JSON.replace("\"0.4.0\",", "\"nightly\",");
        let elsewhere =
            RELEASE_JSON.replace("https://github.com/Veritaris/", "https://example.com/");
        for body in [bad_tag.as_str(), elsewhere.as_str(), "not json", "{}"] {
            assert_eq!(
                evaluate(&ok(body), at(0)),
                Err(Failure::BadAnswer),
                "{body}"
            );
        }
        let http = |status| HttpResponse {
            status,
            ..HttpResponse::default()
        };
        assert_eq!(evaluate(&http(304), at(0)), Ok(Answer::NotModified));
        assert_eq!(
            evaluate(&http(502), at(0)),
            Err(Failure::Http { status: 502 })
        );
        // 403 without the limit's headers is just a status.
        assert_eq!(
            evaluate(&http(403), at(0)),
            Err(Failure::Http { status: 403 })
        );
    }

    #[test]
    fn rate_limits_wait_until_githubs_time() {
        let reset = HttpResponse {
            status: 403,
            ratelimit_reset: Some(1_800_000_900),
            ..HttpResponse::default()
        };
        assert_eq!(
            evaluate(&reset, at(0)),
            Err(Failure::RateLimited { until: at(900) })
        );
        let retry = HttpResponse {
            status: 429,
            retry_after: Some(120),
            ..HttpResponse::default()
        };
        assert_eq!(
            evaluate(&retry, at(10)),
            Err(Failure::RateLimited { until: at(130) })
        );
    }

    #[test]
    fn the_schedule_runs_on_a_fake_clock() {
        let start = at(0);
        // Off (or dev, or the note not shown): never.
        assert_eq!(next_check(false, start, FIRST, None, None), None);
        // 5 s after start, then daily.
        assert_eq!(next_check(true, start, FIRST, None, None), Some(at(5)));
        assert_eq!(
            next_check(true, start, FIRST, Some(at(5)), None),
            Some(at(5) + DAILY)
        );
        // A failure: an hour later.
        let offline = Failure::Offline;
        assert_eq!(
            next_check(true, start, FIRST, Some(at(5)), Some(&offline)),
            Some(at(5) + RETRY)
        );
        // A rate limit: its reset time, also one stored by an earlier run,
        // but never before the first 5 s.
        let limited = Failure::RateLimited { until: at(900) };
        assert_eq!(
            next_check(true, start, FIRST, Some(at(5)), Some(&limited)),
            Some(at(900))
        );
        assert_eq!(
            next_check(true, start, FIRST, None, Some(&limited)),
            Some(at(900))
        );
        let past = Failure::RateLimited {
            until: at(0) - DAILY,
        };
        assert_eq!(
            next_check(true, start, FIRST, None, Some(&past)),
            Some(at(5))
        );
        // An earlier run's failure doesn't delay the first check.
        assert_eq!(
            next_check(true, start, FIRST, None, Some(&offline)),
            Some(at(5))
        );
    }

    #[test]
    fn the_file_round_trips() {
        let file = UpdatesFile {
            checked_at: Some(at(5)),
            succeeded_at: Some(at(5)),
            etag: Some("W/\"e1\"".into()),
            latest: Some(Release {
                version: "0.4.0".into(),
                url: format!("{RELEASE_PAGES}releases/tag/0.4.0"),
                published: Some(at(0)),
            }),
            skipped: Some("0.4.0".into()),
            failure: Some(Failure::RateLimited { until: at(900) }),
        };
        let text = toml::to_string(&file).unwrap();
        assert!(
            text.contains("checked_at = \"2027-01-15T08:00:05Z\""),
            "{text}"
        );
        assert!(text.contains("reason = \"rate-limited\""), "{text}");
        assert_eq!(toml::from_str::<UpdatesFile>(&text).unwrap(), file);
        assert_eq!(
            toml::from_str::<UpdatesFile>("").unwrap(),
            UpdatesFile::default()
        );
    }

    /// Answers with `responses` in turn (the last one again after that) and
    /// records each request.
    #[derive(Clone)]
    struct Canned {
        responses: Arc<Mutex<Vec<Result<HttpResponse, Failure>>>>,
        requests: Arc<Mutex<Vec<HttpRequest>>>,
    }

    impl Canned {
        fn new(responses: Vec<Result<HttpResponse, Failure>>) -> Self {
            Self {
                responses: Arc::new(Mutex::new(responses)),
                requests: Arc::default(),
            }
        }

        fn requests(&self) -> Vec<HttpRequest> {
            self.requests.lock().unwrap().clone()
        }
    }

    impl Transport for Canned {
        fn get(&self, request: &HttpRequest) -> Result<HttpResponse, Failure> {
            self.requests.lock().unwrap().push(request.clone());
            let mut responses = self.responses.lock().unwrap();
            if responses.len() > 1 {
                responses.remove(0)
            } else {
                responses[0].clone()
            }
        }
    }

    struct Run {
        checker: Checker,
        statuses: Receiver<UpdateStatus>,
        _dir: tempfile::TempDir,
        dir: PathBuf,
    }

    impl Run {
        fn until(&self, what: &str, check: impl Fn(&UpdateStatus) -> bool) -> UpdateStatus {
            loop {
                let status = self
                    .statuses
                    .recv_timeout(Duration::from_secs(10))
                    .unwrap_or_else(|_| panic!("timed out waiting for {what}"));
                if check(&status) {
                    return status;
                }
            }
        }

        /// No status but those already sent for `for_`.
        fn quiet(&self, for_: Duration) {
            let deadline = std::time::Instant::now() + for_;
            while let Some(left) = deadline.checked_duration_since(std::time::Instant::now()) {
                if let Ok(status) = self.statuses.recv_timeout(left) {
                    assert!(!status.checking, "checked: {status:?}");
                }
            }
        }
    }

    fn run(version: &str, enabled: bool, noted: bool, transport: impl Transport + 'static) -> Run {
        let dir = tempfile::tempdir().unwrap();
        let app_dir = dir.path().to_path_buf();
        let (tx, statuses) = mpsc::channel();
        let checker = Checker::start(
            app_dir.clone(),
            Options {
                version: version.into(),
                url: "http://127.0.0.1:9/never".into(),
                first: Duration::ZERO,
                enabled,
                noted,
            },
            Box::new(transport),
            move |status| {
                let _ = tx.send(status);
            },
        );
        Run {
            checker,
            statuses,
            _dir: dir,
            dir: app_dir,
        }
    }

    #[test]
    fn dev_builds_off_and_unnoted_never_request_but_check_now_does() {
        for (version, enabled, noted) in [
            ("dev", true, true),
            ("0.3.2", false, true),
            ("0.3.2", true, false),
        ] {
            let canned = Canned::new(vec![Ok(ok(RELEASE_JSON))]);
            let run = run(version, enabled, noted, canned.clone());
            let first = run.until("the first status", |_| true);
            assert!(!first.auto && first.next_check.is_none(), "{first:?}");
            run.quiet(Duration::from_millis(300));
            assert!(canned.requests().is_empty(), "{version} {enabled} {noted}");
            run.checker.send(Request::CheckNow);
            let done = run.until("check now", |s| s.checked_this_run && !s.checking);
            assert_eq!(done.latest.unwrap().version, "0.4.0");
            assert_eq!(canned.requests().len(), 1);
        }
    }

    #[test]
    fn a_check_keeps_its_etag_and_a_304_keeps_the_release() {
        let not_modified = HttpResponse {
            status: 304,
            ..HttpResponse::default()
        };
        let canned = Canned::new(vec![Ok(ok(RELEASE_JSON)), Ok(not_modified)]);
        let run = run("0.3.2", true, true, canned.clone());
        let first = run.until("the first check", |s| s.checked_this_run && !s.checking);
        assert_eq!(first.announced().unwrap().version, "0.4.0");
        assert_eq!(first.next_check, first.checked_at.map(|t| t + DAILY));
        run.checker.send(Request::CheckNow);
        let second = run.until("the second check", |s| {
            !s.checking && s.succeeded_at > first.succeeded_at
        });
        assert_eq!(second.latest, first.latest);
        let requests = canned.requests();
        assert_eq!(requests[0].etag, None);
        assert_eq!(requests[1].etag.as_deref(), Some("\"e1\""));
        assert_eq!(requests[1].user_agent, "sshelf/0.3.2");
        // Kept for the next start.
        let file: UpdatesFile = Store::new(run.dir.clone()).load().data;
        assert_eq!(file.etag.as_deref(), Some("\"e1\""));
        assert_eq!(file.latest, second.latest);
    }

    #[test]
    fn failures_retry_in_an_hour_and_skipping_is_kept() {
        let canned = Canned::new(vec![Err(Failure::Timeout)]);
        let run = run("0.3.2", true, true, canned);
        let failed = run.until("the failure", |s| s.failure.is_some());
        assert_eq!(failed.failure, Some(Failure::Timeout));
        assert_eq!(failed.next_check, failed.checked_at.map(|t| t + RETRY));
        run.checker.send(Request::Skip("0.4.0".into()));
        run.until("skipped", |s| s.skipped.as_deref() == Some("0.4.0"));
        let file: UpdatesFile = Store::new(run.dir.clone()).load().data;
        assert_eq!(file.skipped.as_deref(), Some("0.4.0"));
        assert_eq!(file.failure, Some(Failure::Timeout));
        run.checker.send(Request::Unskip);
        run.until("unskipped", |s| s.skipped.is_none());
    }

    #[test]
    fn turning_it_on_later_checks() {
        let canned = Canned::new(vec![Ok(ok(RELEASE_JSON))]);
        let run = run("0.3.2", true, false, canned.clone());
        run.until("the first status", |_| true);
        run.quiet(Duration::from_millis(200));
        assert!(canned.requests().is_empty());
        // The first-run note answered.
        run.checker.send(Request::Configure {
            enabled: true,
            noted: true,
        });
        run.until("the check", |s| s.checked_this_run && !s.checking);
        assert_eq!(canned.requests().len(), 1);
    }

    /// One HTTP exchange on a local port: the request's head, and `reply`
    /// (nothing at all with `None`, until the client gives up).
    fn serve(reply: Option<&'static str>) -> (String, std::thread::JoinHandle<String>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}/latest", listener.local_addr().unwrap());
        let handle = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut head = String::new();
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            loop {
                let mut line = String::new();
                if reader.read_line(&mut line).unwrap_or(0) == 0 || line == "\r\n" {
                    break;
                }
                head.push_str(&line);
            }
            match reply {
                Some(reply) => stream.write_all(reply.as_bytes()).unwrap(),
                None => thread::sleep(Duration::from_millis(800)),
            }
            head
        });
        (url, handle)
    }

    fn local() -> Ureq {
        Ureq {
            timeout: Duration::from_millis(400),
            env_proxy: false,
        }
    }

    fn request(url: &str, etag: Option<&str>) -> HttpRequest {
        HttpRequest {
            url: url.into(),
            user_agent: "sshelf/0.3.2".into(),
            etag: etag.map(str::to_string),
        }
    }

    #[test]
    fn ureq_sends_the_headers_and_reads_the_answer() {
        let body = RELEASE_JSON;
        let reply: &'static str = Box::leak(
            format!(
                "HTTP/1.1 200 OK\r\nETag: \"e2\"\r\nContent-Type: application/json\r\n\
                 Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            )
            .into_boxed_str(),
        );
        let (url, server) = serve(Some(reply));
        let response = local().get(&request(&url, Some("\"e1\""))).unwrap();
        let head = server.join().unwrap().to_ascii_lowercase();
        assert!(head.starts_with("get /latest http/1.1"), "{head}");
        assert!(head.contains("user-agent: sshelf/0.3.2"), "{head}");
        assert!(
            head.contains("accept: application/vnd.github+json"),
            "{head}"
        );
        assert!(head.contains("if-none-match: \"e1\""), "{head}");
        assert_eq!(response.status, 200);
        assert_eq!(response.etag.as_deref(), Some("\"e2\""));
        assert!(matches!(
            evaluate(&response, SystemTime::now()),
            Ok(Answer::Release { .. })
        ));
    }

    #[test]
    fn ureq_reads_rate_limits_and_statuses() {
        let (url, server) = serve(Some(
            "HTTP/1.1 403 Forbidden\r\nx-ratelimit-remaining: 0\r\nx-ratelimit-reset: 1800000900\r\n\
             Content-Length: 0\r\nConnection: close\r\n\r\n",
        ));
        let response = local().get(&request(&url, None)).unwrap();
        server.join().unwrap();
        assert_eq!(
            evaluate(&response, at(0)),
            Err(Failure::RateLimited { until: at(900) })
        );
        let (url, server) = serve(Some(
            "HTTP/1.1 304 Not Modified\r\nETag: \"e1\"\r\nConnection: close\r\n\r\n",
        ));
        let response = local().get(&request(&url, Some("\"e1\""))).unwrap();
        server.join().unwrap();
        assert_eq!(evaluate(&response, at(0)), Ok(Answer::NotModified));
    }

    #[test]
    fn ureq_times_out_and_reports_no_connection() {
        let (url, server) = serve(None);
        assert_eq!(local().get(&request(&url, None)), Err(Failure::Timeout));
        server.join().unwrap();
        // Nothing listens on a port just released (Windows refuses a
        // local connection only after ~2 s of retries).
        let port = TcpListener::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap()
            .port();
        let url = format!("http://127.0.0.1:{port}/latest");
        let patient = Ureq {
            timeout: Duration::from_secs(8),
            env_proxy: false,
        };
        assert_eq!(patient.get(&request(&url, None)), Err(Failure::Offline));
    }
}
