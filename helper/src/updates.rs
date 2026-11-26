//! Telling the person when a newer Cohelm is out.
//!
//! The Helper reads Cohelm's releases from GitHub now and then and, when one
//! is newer than this Cohelm, the Agents list shows a notice linking to its
//! notes and its download. Nothing is downloaded or installed: the person
//! decides. The request carries nothing about the person beyond what any
//! request to GitHub does.

use std::{sync::Arc, time::Duration};

use semver::Version;
use serde_json::{Value, json};
use tokio::sync::watch;
use tokio_util::sync::CancellationToken;

/// Where Cohelm's releases are published. Links in a release that point
/// anywhere else are ignored.
const RELEASES: &str = "https://github.com/mayes/cohelm/releases/";

/// How the Helper looks for a newer Cohelm.
#[derive(Debug, Clone)]
pub struct UpdateCheck {
    /// The release feed, in the shape of GitHub's list-releases API.
    pub feed: String,
    /// How long after starting the Helper looks first.
    pub first: Duration,
    /// How often it looks after a check that worked.
    pub every: Duration,
    /// How soon it tries again after a check that failed.
    pub retry: Duration,
}

impl UpdateCheck {
    /// Cohelm's own releases on GitHub.
    pub fn github() -> Self {
        Self {
            feed: "https://api.github.com/repos/mayes/cohelm/releases?per_page=20".into(),
            first: Duration::from_secs(10),
            every: Duration::from_secs(12 * 60 * 60),
            retry: Duration::from_secs(30 * 60),
        }
    }
}

/// A newer Cohelm the person can get.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Update {
    pub version: String,
    /// The release's page, with its notes.
    pub notes: String,
    /// The download for this computer, or the release's page when there's
    /// none.
    pub download: String,
}

impl Update {
    pub fn to_json(&self) -> Value {
        json!({ "version": self.version, "notes": self.notes, "download": self.download })
    }
}

/// The download a person on this computer wants from a release.
fn installer_suffix() -> Option<&'static str> {
    if cfg!(target_os = "macos") {
        Some(".dmg")
    } else if cfg!(windows) {
        Some(".exe")
    } else {
        None
    }
}

/// The newest release in `feed` that's newer than `current`, if any.
/// Drafts are skipped, and test versions such as `0.2.0-rc.1` unless this
/// Cohelm is one. GitHub's own pre-release mark doesn't count: the 0.x
/// betas carry it and still need to reach people.
pub fn newest(feed: &Value, current: &Version, suffix: Option<&str>) -> Option<Update> {
    feed.as_array()?
        .iter()
        .filter(|r| r["draft"] != true)
        .filter_map(|r| {
            let tag = r["tag_name"].as_str()?;
            let version = Version::parse(tag.strip_prefix('v').unwrap_or(tag)).ok()?;
            if !version.pre.is_empty() && current.pre.is_empty() {
                return None;
            }
            let notes = r["html_url"].as_str().filter(|u| u.starts_with(RELEASES))?;
            let download = suffix
                .and_then(|suffix| {
                    r["assets"].as_array()?.iter().find_map(|a| {
                        let url = a["browser_download_url"].as_str()?;
                        (url.starts_with(&format!("{RELEASES}download/")) && url.ends_with(suffix))
                            .then_some(url)
                    })
                })
                .unwrap_or(notes);
            Some((version, notes.to_owned(), download.to_owned()))
        })
        .filter(|(version, ..)| version > current)
        .max_by(|a, b| a.0.cmp(&b.0))
        .map(|(version, notes, download)| Update {
            version: version.to_string(),
            notes,
            download,
        })
}

/// Look for a newer Cohelm until `cancel`, telling `found` about it.
pub(crate) async fn run(
    check: UpdateCheck,
    found: Arc<watch::Sender<Option<Update>>>,
    cancel: CancellationToken,
) {
    let current = Version::parse(env!("CARGO_PKG_VERSION")).expect("the Helper's version");
    let client = match reqwest::Client::builder()
        .user_agent(concat!("Cohelm/", env!("CARGO_PKG_VERSION")))
        .timeout(Duration::from_secs(30))
        .build()
    {
        Ok(client) => client,
        Err(e) => {
            tracing::warn!("can't look for Cohelm updates: {e}");
            return;
        }
    };
    let mut wait = check.first;
    loop {
        tokio::select! {
            () = cancel.cancelled() => return,
            () = tokio::time::sleep(wait) => {}
        }
        wait = match fetch(&client, &check.feed).await {
            Ok(feed) => {
                let update = newest(&feed, &current, installer_suffix());
                if let Some(update) = &update
                    && found.borrow().as_ref() != Some(update)
                {
                    tracing::info!("Cohelm {} is out: {}", update.version, update.notes);
                }
                found.send_if_modified(|seen| {
                    let changed = *seen != update;
                    *seen = update;
                    changed
                });
                check.every
            }
            Err(e) => {
                // Offline, or GitHub is busy: say nothing and try later.
                tracing::debug!("couldn't look for Cohelm updates: {e}");
                check.retry
            }
        };
    }
}

async fn fetch(client: &reqwest::Client, feed: &str) -> anyhow::Result<Value> {
    let body = client
        .get(feed)
        .header("Accept", "application/vnd.github+json")
        .send()
        .await?
        .error_for_status()?
        .bytes()
        .await?;
    let feed: Value = serde_json::from_slice(&body)?;
    // Anything but a list is GitHub saying something went wrong.
    anyhow::ensure!(feed.is_array(), "not a list of releases");
    Ok(feed)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn release(tag: &str) -> Value {
        json!({
            "tag_name": tag,
            "draft": false,
            "prerelease": false,
            "html_url": format!("{RELEASES}tag/{tag}"),
            "assets": [
                { "browser_download_url": format!("{RELEASES}download/{tag}/Cohelm-{tag}.dmg") },
                { "browser_download_url": format!("{RELEASES}download/{tag}/Cohelm-{tag}-setup.exe") },
            ],
        })
    }

    fn v(s: &str) -> Version {
        Version::parse(s).unwrap()
    }

    #[test]
    fn a_newer_release_links_its_notes_and_download() {
        let feed = json!([release("v0.1.0"), release("v0.2.0"), release("v0.1.5")]);
        let update = newest(&feed, &v("0.1.0"), Some(".dmg")).unwrap();
        assert_eq!(update.version, "0.2.0");
        assert_eq!(update.notes, format!("{RELEASES}tag/v0.2.0"));
        assert_eq!(
            update.download,
            format!("{RELEASES}download/v0.2.0/Cohelm-v0.2.0.dmg")
        );
        let windows = newest(&feed, &v("0.1.0"), Some(".exe")).unwrap();
        assert!(windows.download.ends_with("-setup.exe"));
        // Nothing for this computer: the release's page.
        assert_eq!(
            newest(&feed, &v("0.1.0"), None).unwrap().download,
            update.notes
        );
    }

    #[test]
    fn no_notice_when_this_cohelm_is_current_or_newer() {
        let feed = json!([release("v0.1.0"), release("v0.0.9")]);
        assert_eq!(newest(&feed, &v("0.1.0"), Some(".dmg")), None);
        assert_eq!(newest(&feed, &v("0.3.0"), Some(".dmg")), None);
        assert_eq!(newest(&json!([]), &v("0.1.0"), Some(".dmg")), None);
        assert_eq!(
            newest(&json!({"message": "rate limited"}), &v("0.1.0"), None),
            None
        );
    }

    #[test]
    fn drafts_test_releases_and_stray_links_are_skipped() {
        let mut draft = release("v0.4.0");
        draft["draft"] = json!(true);
        let mut beta = release("v0.3.0-beta.1");
        beta["prerelease"] = json!(true);
        let mut elsewhere = release("v0.5.0");
        elsewhere["html_url"] = json!("https://example.com/cohelm/releases/v0.5.0");
        let mut odd_download = release("v0.2.0");
        odd_download["assets"] =
            json!([{ "browser_download_url": "https://example.com/Cohelm.dmg" }]);
        let feed = json!([
            draft,
            beta,
            elsewhere,
            odd_download,
            release("not-a-version")
        ]);
        // Marked as a pre-release on GitHub, but a plain version: it counts.
        let mut marked = release("v0.1.5");
        marked["prerelease"] = json!(true);
        assert_eq!(
            newest(&json!([marked]), &v("0.1.0"), None).unwrap().version,
            "0.1.5"
        );

        let update = newest(&feed, &v("0.1.0"), Some(".dmg")).unwrap();
        assert_eq!(update.version, "0.2.0");
        assert_eq!(update.download, update.notes, "only Cohelm's own downloads");
        // A test build hears about newer test builds.
        let update = newest(&feed, &v("0.1.0-beta.1"), Some(".dmg")).unwrap();
        assert_eq!(update.version, "0.3.0-beta.1");
        // And about the release it led to.
        let feed = json!([release("v0.1.0")]);
        assert_eq!(
            newest(&feed, &v("0.1.0-beta.2"), None).unwrap().version,
            "0.1.0"
        );
    }

    #[tokio::test]
    async fn a_failed_check_is_quiet_and_tried_again() {
        use std::sync::atomic::{AtomicU32, Ordering};

        use axum::{Json, Router, http::StatusCode, routing::get};

        let calls = Arc::new(AtomicU32::new(0));
        let seen = calls.clone();
        let app = Router::new().route(
            "/releases",
            get(move || {
                let n = seen.fetch_add(1, Ordering::SeqCst);
                async move {
                    if n == 0 {
                        Err(StatusCode::SERVICE_UNAVAILABLE)
                    } else {
                        Ok(Json(json!([release("v99.0.0")])))
                    }
                }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

        let found = Arc::new(watch::Sender::new(None));
        let mut seen_update = found.subscribe();
        let cancel = CancellationToken::new();
        let check = UpdateCheck {
            feed: format!("http://{addr}/releases"),
            first: Duration::ZERO,
            every: Duration::from_secs(3600),
            retry: Duration::from_millis(100),
        };
        tokio::spawn(run(check, found, cancel.clone()));
        tokio::time::timeout(
            Duration::from_secs(10),
            seen_update.wait_for(Option::is_some),
        )
        .await
        .expect("found after a retry")
        .unwrap();
        assert_eq!(seen_update.borrow().as_ref().unwrap().version, "99.0.0");
        assert_eq!(calls.load(Ordering::SeqCst), 2);
        cancel.cancel();
    }
}
