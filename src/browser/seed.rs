//! Moving a login from one machine to another as an environment variable.
//!
//! `export-session` signs in on a machine with a screen and writes the profile's
//! cookies to a file; a headless deployment gets that file's content as
//! `K_RUOKA_SESSION` and loads it into its own Chrome on launch. From then on the
//! running browser renews the session itself, in its own profile.
//!
//! A seed is applied once, not on every launch: by the next restart the profile holds
//! newer cookies than the seed, and re-applying would roll them back. The last applied
//! seed is remembered next to the profile, so only a *different* seed (a fresh export)
//! is applied again.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use chromiumoxide::cdp::browser_protocol::network::{
    Cookie, CookieParam, CookieSameSite, TimeSinceEpoch,
};
use serde::{Deserialize, Serialize};

pub const SEED_ENV: &str = "K_RUOKA_SESSION";

const FORMAT_VERSION: u32 = 1;

#[derive(Debug, Serialize, Deserialize)]
struct Seed {
    v: u32,
    cookies: Vec<SeedCookie>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
struct SeedCookie {
    name: String,
    value: String,
    domain: String,
    path: String,
    /// `None` for a session cookie (CDP reports those with `expires: -1`).
    expires: Option<f64>,
    http_only: bool,
    secure: bool,
    same_site: Option<String>,
}

impl From<&Cookie> for SeedCookie {
    fn from(c: &Cookie) -> Self {
        Self {
            name: c.name.clone(),
            value: c.value.clone(),
            domain: c.domain.clone(),
            path: c.path.clone(),
            expires: (!c.session && c.expires > 0.0).then_some(c.expires),
            http_only: c.http_only,
            secure: c.secure,
            same_site: c.same_site.as_ref().map(|s| s.as_ref().to_owned()),
        }
    }
}

impl SeedCookie {
    fn into_param(self) -> CookieParam {
        let mut param = CookieParam::new(self.name, self.value);
        param.domain = Some(self.domain);
        param.path = Some(self.path);
        param.secure = Some(self.secure);
        param.http_only = Some(self.http_only);
        param.expires = self.expires.map(TimeSinceEpoch::new);
        param.same_site = self.same_site.as_deref().and_then(|s| match s {
            "Strict" => Some(CookieSameSite::Strict),
            "Lax" => Some(CookieSameSite::Lax),
            "None" => Some(CookieSameSite::None),
            _ => None,
        });
        param
    }
}

/// One line, safe to paste into an env file or a stack variable.
pub fn encode(cookies: &[Cookie]) -> Result<String> {
    let seed = Seed {
        v: FORMAT_VERSION,
        cookies: cookies.iter().map(SeedCookie::from).collect(),
    };
    Ok(STANDARD.encode(serde_json::to_vec(&seed)?))
}

pub fn decode(raw: &str) -> Result<Vec<CookieParam>> {
    let bytes = STANDARD
        .decode(raw.trim())
        .context("K_RUOKA_SESSION is not base64 from `export-session`")?;
    let seed: Seed = serde_json::from_slice(&bytes)
        .context("K_RUOKA_SESSION does not hold an exported session")?;
    if seed.v != FORMAT_VERSION {
        bail!(
            "K_RUOKA_SESSION has format v{}, this build reads v{FORMAT_VERSION}",
            seed.v
        );
    }
    if seed.cookies.is_empty() {
        bail!("K_RUOKA_SESSION holds no cookies");
    }
    Ok(seed
        .cookies
        .into_iter()
        .map(SeedCookie::into_param)
        .collect())
}

/// Beside the profile, like `default_store`. It holds a credential, as the profile does.
pub fn marker_path(profile: &Path) -> PathBuf {
    profile
        .parent()
        .unwrap_or(profile)
        .join("applied_session_seed")
}

/// The seed to apply now: set, and not the one applied last time.
pub fn pending(profile: &Path) -> Option<String> {
    let seed = std::env::var(SEED_ENV).ok()?.trim().to_owned();
    if seed.is_empty() {
        return None;
    }
    let applied = std::fs::read_to_string(marker_path(profile)).unwrap_or_default();
    (applied.trim() != seed).then_some(seed)
}

pub fn mark_applied(profile: &Path, seed: &str) -> Result<()> {
    write_private(&marker_path(profile), seed)
}

/// Where `export-session` writes, beside the profile it signed in.
pub fn export_path(profile: &Path) -> PathBuf {
    profile.parent().unwrap_or(profile).join("session_export")
}

pub fn write_private(path: &Path, content: &str) -> Result<()> {
    #[cfg(unix)]
    {
        use std::io::Write;
        use std::os::unix::fs::OpenOptionsExt;
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(path)
            .with_context(|| format!("writing {}", path.display()))?;
        file.write_all(content.as_bytes())?;
        Ok(())
    }
    #[cfg(not(unix))]
    {
        std::fs::write(path, content).with_context(|| format!("writing {}", path.display()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cookie(
        name: &str,
        session: bool,
        expires: f64,
        same_site: Option<CookieSameSite>,
    ) -> Cookie {
        let mut raw = serde_json::json!({
            "name": name, "value": format!("{name}-value"), "domain": ".k-ruoka.fi",
            "path": "/", "expires": expires, "size": 10, "httpOnly": true, "secure": true,
            "session": session, "priority": "Medium", "sourceScheme": "Secure",
            "sourcePort": 443,
        });
        // CDP omits sameSite rather than sending null.
        if let Some(s) = same_site {
            raw["sameSite"] = s.as_ref().into();
        }
        serde_json::from_value(raw).unwrap()
    }

    #[test]
    fn a_round_trip_keeps_what_a_login_needs() {
        let cookies = vec![
            cookie("auth", false, 1_900_000_000.0, Some(CookieSameSite::Lax)),
            cookie("sid", true, -1.0, None),
        ];
        let params = decode(&encode(&cookies).unwrap()).unwrap();
        assert_eq!(params.len(), 2);
        assert_eq!(params[0].name, "auth");
        assert_eq!(params[0].value, "auth-value");
        assert_eq!(params[0].domain.as_deref(), Some(".k-ruoka.fi"));
        assert_eq!(
            params[0].expires.as_ref().map(|e| *e.inner()),
            Some(1_900_000_000.0)
        );
        assert_eq!(params[0].same_site, Some(CookieSameSite::Lax));
        assert_eq!(params[0].http_only, Some(true));
        // A session cookie must stay one: an `expires` of -1 would delete it on set.
        assert!(params[1].expires.is_none());
    }

    #[test]
    fn garbage_is_refused_rather_than_applied() {
        assert!(decode("not base64 !!").is_err());
        assert!(decode(&STANDARD.encode(b"{\"v\":1,\"cookies\":[]}")).is_err());
        assert!(decode(&STANDARD.encode(b"{\"v\":2,\"cookies\":[]}")).is_err());
    }

    #[test]
    fn a_seed_is_pending_only_until_applied() {
        let dir = std::env::temp_dir().join(format!("k-ruoka-seed-test-{}", std::process::id()));
        let profile = dir.join("profile");
        std::fs::create_dir_all(&profile).unwrap();
        // SAFETY: this test is the only one in the crate touching this variable.
        unsafe { std::env::set_var(SEED_ENV, "seed-one") };
        assert_eq!(pending(&profile).as_deref(), Some("seed-one"));
        mark_applied(&profile, "seed-one").unwrap();
        assert_eq!(
            pending(&profile),
            None,
            "a restart must not roll renewed cookies back"
        );
        unsafe { std::env::set_var(SEED_ENV, "seed-two") };
        assert_eq!(
            pending(&profile).as_deref(),
            Some("seed-two"),
            "a fresh export applies"
        );
        unsafe { std::env::remove_var(SEED_ENV) };
        assert_eq!(pending(&profile), None);
        std::fs::remove_dir_all(&dir).ok();
    }
}
