//! Alko catalogue access through the website's guest-session HTTP API.
//!
//! This is intentionally independent from the K-Ruoka Chrome profile. Alko's website
//! issues a short-lived NextAuth guest session, after which product and store lookups are
//! ordinary JSON HTTP calls. No account login and no browser process are required.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, anyhow, bail};
use reqwest::header::{ACCEPT, COOKIE, SET_COOKIE, USER_AGENT};
use reqwest::{Client, Method, StatusCode};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use tokio::sync::Mutex;

const BASE_URL: &str = "https://www.alko.fi";
const SESSION_REFRESH_AFTER: Duration = Duration::from_secs(25 * 60);
const DEFAULT_LIMIT: u32 = 10;
const MAX_LIMIT: u32 = 50;
const USER_AGENT_VALUE: &str =
    "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/150.0.0.0 Safari/537.36";

#[derive(Default)]
struct SessionState {
    cookies: HashMap<String, String>,
    created_at: Option<Instant>,
}

impl SessionState {
    fn is_fresh(&self) -> bool {
        self.created_at
            .is_some_and(|created| created.elapsed() < SESSION_REFRESH_AFTER)
            && !self.cookies.is_empty()
    }

    fn cookie_header(&self) -> String {
        self.cookies
            .iter()
            .map(|(name, value)| format!("{name}={value}"))
            .collect::<Vec<_>>()
            .join("; ")
    }

    fn absorb_set_cookies(&mut self, headers: &reqwest::header::HeaderMap) -> Result<()> {
        for raw in headers.get_all(SET_COOKIE).iter() {
            let raw = raw.to_str().context("Alko returned a non-UTF8 Set-Cookie header")?;
            let Some(name_value) = raw.split(';').next() else {
                continue;
            };
            let Some((name, value)) = name_value.split_once('=') else {
                continue;
            };
            self.cookies
                .insert(name.trim().to_string(), value.trim().to_string());
        }
        Ok(())
    }
}

pub struct AlkoClient {
    http: Client,
    session: Mutex<SessionState>,
}

impl Default for AlkoClient {
    fn default() -> Self {
        Self::new()
    }
}

impl AlkoClient {
    pub fn new() -> Self {
        Self {
            http: Client::new(),
            session: Mutex::new(SessionState::default()),
        }
    }

    async fn bootstrap_session(&self, state: &mut SessionState) -> Result<()> {
        state.cookies.clear();
        state.created_at = None;

        let csrf_response = self
            .http
            .get(format!("{BASE_URL}/api/auth/csrf"))
            .header(USER_AGENT, USER_AGENT_VALUE)
            .header(ACCEPT, "application/json")
            .send()
            .await
            .context("Alko CSRF request failed")?;

        if !csrf_response.status().is_success() {
            bail!("Alko CSRF request returned HTTP {}", csrf_response.status());
        }
        state.absorb_set_cookies(csrf_response.headers())?;
        let csrf: CsrfResponse = csrf_response
            .json()
            .await
            .context("Alko CSRF response had an unexpected shape")?;

        let login_response = self
            .http
            .post(format!("{BASE_URL}/api/auth/callback/credentials"))
            .header(USER_AGENT, USER_AGENT_VALUE)
            .header(COOKIE, state.cookie_header())
            .form(&[
                ("redirect", "false"),
                ("csrfToken", csrf.csrf_token.as_str()),
                ("callbackUrl", "https://www.alko.fi/fi/tuotteet"),
                ("json", "true"),
            ])
            .send()
            .await
            .context("Alko guest-session bootstrap failed")?;

        if login_response.status().is_client_error() || login_response.status().is_server_error() {
            bail!(
                "Alko guest-session bootstrap returned HTTP {}",
                login_response.status()
            );
        }
        state.absorb_set_cookies(login_response.headers())?;

        if !state
            .cookies
            .contains_key("__Secure-next-auth.session-token")
        {
            bail!("Alko guest-session bootstrap returned no NextAuth session token");
        }

        state.created_at = Some(Instant::now());
        Ok(())
    }

    async fn session_cookie(&self) -> Result<String> {
        let mut state = self.session.lock().await;
        if !state.is_fresh() {
            self.bootstrap_session(&mut state).await?;
        }
        Ok(state.cookie_header())
    }

    async fn invalidate_session(&self) {
        let mut state = self.session.lock().await;
        state.cookies.clear();
        state.created_at = None;
    }

    async fn request_json(
        &self,
        method: Method,
        path: &str,
        body: Option<&serde_json::Value>,
    ) -> Result<serde_json::Value> {
        for attempt in 0..2 {
            let cookies = self.session_cookie().await?;
            let mut request = self
                .http
                .request(method.clone(), format!("{BASE_URL}{path}"))
                .header(USER_AGENT, USER_AGENT_VALUE)
                .header(ACCEPT, "application/json")
                .header(COOKIE, cookies);
            if let Some(body) = body {
                request = request.json(body);
            }

            let response = request
                .send()
                .await
                .with_context(|| format!("Alko request failed: {method} {path}"))?;

            if matches!(response.status(), StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN)
                && attempt == 0
            {
                self.invalidate_session().await;
                continue;
            }

            if !response.status().is_success() {
                bail!("Alko API returned HTTP {} for {path}", response.status());
            }

            return response
                .json()
                .await
                .with_context(|| format!("Alko API returned invalid JSON for {path}"));
        }

        Err(anyhow!("Alko session refresh did not recover the request"))
    }

    pub async fn search_products(
        &self,
        query: &str,
        store_id: Option<&str>,
        limit: Option<u32>,
    ) -> Result<AlkoProductSearchView> {
        let query = query.trim();
        if query.is_empty() {
            bail!("query must not be empty");
        }
        let limit = limit.unwrap_or(DEFAULT_LIMIT).clamp(1, MAX_LIMIT);

        let mut body = serde_json::json!({
            "top": limit,
            "skip": 0,
            "search": query,
        });
        if let Some(store_id) = store_id.filter(|value| !value.trim().is_empty()) {
            body["storeId"] = serde_json::Value::String(store_id.to_string());
        }

        let raw = self
            .request_json(Method::POST, "/api/search/product?lang=fi", Some(&body))
            .await?;
        let parsed: ProductSearchResponse = serde_json::from_value(raw)
            .context("Alko product-search response had an unexpected shape")?;

        let total_hits = parsed.total.unwrap_or(parsed.value.len() as u64);
        Ok(AlkoProductSearchView {
            total_hits,
            results: parsed.value.into_iter().map(Into::into).collect(),
        })
    }

    pub async fn stores(&self, city: Option<&str>) -> Result<Vec<AlkoStoreView>> {
        let raw = self.request_json(Method::GET, "/api/stores", None).await?;
        let parsed: StoresResponse =
            serde_json::from_value(raw).context("Alko stores response had an unexpected shape")?;

        let city = city.map(str::trim).filter(|value| !value.is_empty());
        Ok(parsed
            .data
            .into_iter()
            .filter(|store| {
                city.is_none_or(|needle| store.city.to_lowercase().contains(&needle.to_lowercase()))
            })
            .map(Into::into)
            .collect())
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct CsrfResponse {
    csrf_token: String,
}

#[derive(Deserialize)]
struct ProductSearchResponse {
    #[serde(rename = "@odata.count")]
    total: Option<u64>,
    value: Vec<AlkoProduct>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct AlkoProduct {
    id: String,
    name: String,
    price: Option<f64>,
    #[serde(default)]
    abv: Option<f64>,
    #[serde(default)]
    volume: Option<f64>,
    #[serde(default)]
    country_name: Option<String>,
    #[serde(default)]
    product_group_name: Vec<String>,
}

#[derive(Debug, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct AlkoProductSearchView {
    pub total_hits: u64,
    pub results: Vec<AlkoProductView>,
}

#[derive(Debug, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct AlkoProductView {
    pub sku: String,
    pub name: String,
    pub price: Option<f64>,
    pub price_per_litre: Option<f64>,
    pub abv: Option<f64>,
    pub country: Option<String>,
    pub category: Option<String>,
    pub image_url: String,
}

impl From<AlkoProduct> for AlkoProductView {
    fn from(product: AlkoProduct) -> Self {
        let price_per_litre = match (product.price, product.volume) {
            (Some(price), Some(volume)) if volume > 0.0 => Some(price / volume),
            _ => None,
        };
        let sku = product.id;
        Self {
            image_url: format!(
                "https://images.alko.fi/images/cs_srgb,f_auto,t_products/cdn/{sku}/{sku}.jpg"
            ),
            sku,
            name: product.name,
            price: product.price,
            price_per_litre,
            abv: product.abv,
            country: product.country_name,
            category: product.product_group_name.into_iter().next(),
        }
    }
}

#[derive(Deserialize)]
struct StoresResponse {
    data: Vec<AlkoStore>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct AlkoStore {
    id: String,
    name: String,
    city: String,
    address: String,
    postal_code: String,
}

#[derive(Debug, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct AlkoStoreView {
    pub store_id: String,
    pub name: String,
    pub city: String,
    pub address: String,
    pub postal_code: String,
}

impl From<AlkoStore> for AlkoStoreView {
    fn from(store: AlkoStore) -> Self {
        Self {
            store_id: store.id,
            name: store.name,
            city: store.city,
            address: store.address,
            postal_code: store.postal_code,
        }
    }
}
