//! S-Kaupat catalogue access through the website's persisted GraphQL queries.
//!
//! The GraphQL operation hashes are deployment artefacts of the S-Kaupat frontend rather
//! than stable API identifiers. We therefore learn them from the site's own network
//! traffic with the existing Chrome session, cache them in memory, and re-discover once
//! if the API reports `PERSISTED_QUERY_NOT_FOUND`. Product/store data itself is fetched
//! directly with reqwest; Chrome is not kept on the hot path.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail};
use chromiumoxide::Page;
use chromiumoxide::cdp::browser_protocol::network::EventRequestWillBeSent;
use futures::StreamExt;
use reqwest::header::{ACCEPT, ORIGIN, REFERER, USER_AGENT};
use reqwest::{Client, Url};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use tokio::sync::Mutex;

use crate::browser::Session;

const WEB_ORIGIN: &str = "https://www.s-kaupat.fi";
const API_URL: &str = "https://api.s-kaupat.fi/";
const REQUEST_TIMEOUT: Duration = Duration::from_secs(15);
const HASH_DISCOVERY_TIMEOUT: Duration = Duration::from_secs(20);
const STORE_TRIGGER_TIMEOUT: Duration = Duration::from_secs(10);
const DEFAULT_LIMIT: u32 = 10;
const MAX_LIMIT: u32 = 50;
const MAX_STORE_PAGES: usize = 50;
const USER_AGENT_VALUE: &str =
    "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/150.0.0.0 Safari/537.36";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum Operation {
    Products,
    Stores,
}

impl Operation {
    fn name(self) -> &'static str {
        match self {
            Self::Products => "RemoteFilteredProducts",
            Self::Stores => "RemoteStoreSearch",
        }
    }

    fn discovery_url(self) -> &'static str {
        match self {
            Self::Products => "https://www.s-kaupat.fi/hakutulokset?queryString=test",
            Self::Stores => "https://www.s-kaupat.fi/myymalat/prisma",
        }
    }
}

#[derive(Deserialize)]
struct PersistedQueryEnvelope {
    #[serde(rename = "persistedQuery")]
    persisted_query: PersistedQuery,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct PersistedQuery {
    sha256_hash: String,
}

pub struct SKaupatClient {
    http: Client,
    browser: Arc<Session>,
    hashes: Mutex<HashMap<Operation, String>>,
    discovery_gate: Mutex<()>,
}

impl SKaupatClient {
    pub fn new(browser: Arc<Session>) -> Self {
        let http = Client::builder()
            .timeout(REQUEST_TIMEOUT)
            .build()
            .expect("static reqwest client configuration should be valid");
        Self {
            http,
            browser,
            hashes: Mutex::new(HashMap::new()),
            discovery_gate: Mutex::new(()),
        }
    }

    async fn hash_for(&self, operation: Operation) -> Result<String> {
        if let Some(hash) = self.hashes.lock().await.get(&operation).cloned() {
            return Ok(hash);
        }

        let _gate = self.discovery_gate.lock().await;
        if let Some(hash) = self.hashes.lock().await.get(&operation).cloned() {
            return Ok(hash);
        }

        let hash = self.discover_hash(operation).await?;
        self.hashes.lock().await.insert(operation, hash.clone());
        Ok(hash)
    }

    async fn invalidate_hash(&self, operation: Operation) {
        self.hashes.lock().await.remove(&operation);
    }

    async fn discover_hash(&self, operation: Operation) -> Result<String> {
        let page = self
            .browser
            .open_extra_page("about:blank")
            .await
            .context("opening S-Kaupat hash-discovery tab")?;

        let result = discover_hash_on_page(&page, operation).await;
        page.close().await.ok();
        result
    }

    async fn graphql_get(
        &self,
        operation: Operation,
        variables: &serde_json::Value,
        hash: &str,
    ) -> Result<serde_json::Value> {
        let mut url = Url::parse(API_URL).expect("static S-Kaupat API URL is valid");
        let extensions = serde_json::json!({
            "persistedQuery": {
                "version": 1,
                "sha256Hash": hash,
            }
        });
        url.query_pairs_mut()
            .append_pair("operationName", operation.name())
            .append_pair("variables", &variables.to_string())
            .append_pair("extensions", &extensions.to_string());

        let response = self
            .http
            .get(url)
            .header(ORIGIN, WEB_ORIGIN)
            .header(REFERER, format!("{WEB_ORIGIN}/"))
            .header(USER_AGENT, USER_AGENT_VALUE)
            .header(ACCEPT, "application/json")
            .send()
            .await
            .with_context(|| format!("S-Kaupat {} request failed", operation.name()))?;

        if !response.status().is_success() {
            bail!(
                "S-Kaupat {} returned HTTP {}",
                operation.name(),
                response.status()
            );
        }

        response
            .json()
            .await
            .with_context(|| format!("S-Kaupat {} returned invalid JSON", operation.name()))
    }

    async fn graphql_with_hash_retry(
        &self,
        operation: Operation,
        variables: &serde_json::Value,
    ) -> Result<serde_json::Value> {
        for attempt in 0..2 {
            let hash = self.hash_for(operation).await?;
            let raw = self.graphql_get(operation, variables, &hash).await?;
            if persisted_query_not_found(&raw) && attempt == 0 {
                self.invalidate_hash(operation).await;
                continue;
            }
            if persisted_query_not_found(&raw) {
                bail!(
                    "S-Kaupat rejected a freshly discovered {} persisted-query hash",
                    operation.name()
                );
            }
            return Ok(raw);
        }
        Err(anyhow!("S-Kaupat persisted-query recovery exhausted"))
    }

    pub async fn search_products(
        &self,
        query: &str,
        store_id: &str,
        limit: Option<u32>,
    ) -> Result<SKaupatProductSearchView> {
        let query = query.trim();
        if query.is_empty() {
            bail!("query must not be empty");
        }
        let store_id = store_id.trim();
        if store_id.is_empty() {
            bail!("store_id must not be empty");
        }
        let limit = limit.unwrap_or(DEFAULT_LIMIT).clamp(1, MAX_LIMIT);
        let variables = serde_json::json!({
            "queryString": query,
            "storeId": store_id,
            "from": 0,
            "limit": limit,
        });

        let raw = self
            .graphql_with_hash_retry(Operation::Products, &variables)
            .await?;
        let parsed: ProductSearchResponse = serde_json::from_value(raw)
            .context("S-Kaupat product-search response had an unexpected shape")?;
        let products = parsed.data.store.products;
        Ok(SKaupatProductSearchView {
            total_hits: products.total,
            results: products
                .product_list_items
                .into_iter()
                .map(Into::into)
                .collect(),
        })
    }

    pub async fn search_stores(&self, query: Option<&str>) -> Result<SKaupatStoreSearchView> {
        let query = query.map(str::trim).filter(|value| !value.is_empty());
        let mut cursor: Option<String> = None;
        let mut results = Vec::new();

        for _ in 0..MAX_STORE_PAGES {
            let variables = serde_json::json!({
                "query": query,
                "brand": serde_json::Value::Null,
                "cursor": cursor,
            });
            let raw = self
                .graphql_with_hash_retry(Operation::Stores, &variables)
                .await?;
            let parsed: StoreSearchResponse = serde_json::from_value(raw)
                .context("S-Kaupat store-search response had an unexpected shape")?;
            let page = parsed.data.search_stores;
            let total_hits = page.total_count;
            let next_cursor = page.cursor;
            results.extend(page.stores.into_iter().map(Into::into));
            if next_cursor.is_none() {
                return Ok(SKaupatStoreSearchView {
                    total_hits,
                    results,
                });
            }
            cursor = next_cursor;
        }

        bail!("S-Kaupat store search exceeded {MAX_STORE_PAGES} pages")
    }
}

async fn discover_hash_on_page(page: &Page, operation: Operation) -> Result<String> {
    let mut events = page
        .event_listener::<EventRequestWillBeSent>()
        .await
        .context("registering S-Kaupat network listener")?;

    page.goto(operation.discovery_url())
        .await
        .with_context(|| format!("opening S-Kaupat page for {} discovery", operation.name()))?;

    if operation == Operation::Stores {
        trigger_store_search(page).await?;
    }

    let expected = operation.name();
    let listen = async {
        while let Some(event) = events.next().await {
            if let Some(hash) = hash_from_request_url(&event.request.url, expected)? {
                return Ok(hash);
            }
        }
        Err(anyhow!("S-Kaupat network event stream ended during hash discovery"))
    };

    tokio::time::timeout(HASH_DISCOVERY_TIMEOUT, listen)
        .await
        .with_context(|| format!("timed out discovering S-Kaupat {expected} hash"))?
}

async fn trigger_store_search(page: &Page) -> Result<()> {
    let trigger = async {
        loop {
            let clicked: bool = page
                .evaluate(
                    "(() => { \
                       document.getElementById('usercentrics-root')?.remove(); \
                       const b=[...document.querySelectorAll('button')].find(x => \
                         (x.textContent || '').includes('Näytä lisää')); \
                       if (!b) return false; b.click(); return true; \
                     })()",
                )
                .await
                .context("probing S-Kaupat stores page")?
                .into_value()
                .context("S-Kaupat stores-page trigger returned a non-boolean value")?;
            if clicked {
                return Ok(());
            }
            tokio::time::sleep(Duration::from_millis(200)).await;
        }
    };

    tokio::time::timeout(STORE_TRIGGER_TIMEOUT, trigger)
        .await
        .context("S-Kaupat stores page never rendered the 'Näytä lisää' trigger")?
}

fn hash_from_request_url(raw: &str, expected_operation: &str) -> Result<Option<String>> {
    let url = match Url::parse(raw) {
        Ok(url) => url,
        Err(_) => return Ok(None),
    };
    if url.host_str() != Some("api.s-kaupat.fi") {
        return Ok(None);
    }

    let mut operation = None;
    let mut extensions = None;
    for (key, value) in url.query_pairs() {
        match key.as_ref() {
            "operationName" => operation = Some(value.into_owned()),
            "extensions" => extensions = Some(value.into_owned()),
            _ => {}
        }
    }
    if operation.as_deref() != Some(expected_operation) {
        return Ok(None);
    }
    let Some(extensions) = extensions else {
        return Ok(None);
    };
    let parsed: PersistedQueryEnvelope = serde_json::from_str(&extensions)
        .context("S-Kaupat request carried malformed persisted-query metadata")?;
    Ok(Some(parsed.persisted_query.sha256_hash))
}

fn persisted_query_not_found(value: &serde_json::Value) -> bool {
    value
        .get("errors")
        .and_then(serde_json::Value::as_array)
        .is_some_and(|errors| {
            errors.iter().any(|error| {
                error
                    .get("extensions")
                    .and_then(|extensions| extensions.get("code"))
                    .and_then(serde_json::Value::as_str)
                    == Some("PERSISTED_QUERY_NOT_FOUND")
            })
        })
}

#[derive(Deserialize)]
struct ProductSearchResponse {
    data: ProductSearchData,
}

#[derive(Deserialize)]
struct ProductSearchData {
    store: ProductStore,
}

#[derive(Deserialize)]
struct ProductStore {
    products: ProductsConnection,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ProductsConnection {
    total: u64,
    product_list_items: Vec<ProductListItem>,
}

#[derive(Deserialize)]
struct ProductListItem {
    product: SKaupatProduct,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct SKaupatProduct {
    name: String,
    ean: String,
    #[serde(default)]
    price: Option<f64>,
    #[serde(default)]
    brand_name: Option<String>,
    pricing: Pricing,
    product_details: ProductDetails,
    #[serde(default)]
    hierarchy_path: Vec<HierarchyItem>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Pricing {
    #[serde(default)]
    current_price: Option<f64>,
    #[serde(default)]
    comparison_price: Option<f64>,
    #[serde(default)]
    comparison_unit: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ProductDetails {
    product_images: ProductImages,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ProductImages {
    #[serde(default)]
    main_image: Option<ProductImage>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ProductImage {
    url_template: String,
}

#[derive(Deserialize)]
struct HierarchyItem {
    name: String,
}

#[derive(Debug, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct SKaupatProductSearchView {
    pub total_hits: u64,
    pub results: Vec<SKaupatProductView>,
}

#[derive(Debug, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct SKaupatProductView {
    pub ean: String,
    pub name: String,
    pub price: Option<f64>,
    pub comparison_price: Option<f64>,
    pub comparison_unit: Option<String>,
    pub brand: Option<String>,
    pub category: Option<String>,
    pub image_url: Option<String>,
}

impl From<ProductListItem> for SKaupatProductView {
    fn from(item: ProductListItem) -> Self {
        let product = item.product;
        let image_url = product
            .product_details
            .product_images
            .main_image
            .map(|image| {
                image
                    .url_template
                    .replace("{MODIFIERS}", "w_400,h_400")
                    .replace("{EXTENSION}", "png")
            });
        Self {
            ean: product.ean,
            name: product.name,
            price: product.pricing.current_price.or(product.price),
            comparison_price: product.pricing.comparison_price,
            comparison_unit: product.pricing.comparison_unit,
            brand: product.brand_name,
            category: product.hierarchy_path.into_iter().next().map(|item| item.name),
            image_url,
        }
    }
}

#[derive(Deserialize)]
struct StoreSearchResponse {
    data: StoreSearchData,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct StoreSearchData {
    search_stores: StoreSearchConnection,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct StoreSearchConnection {
    total_count: u64,
    #[serde(default)]
    cursor: Option<String>,
    stores: Vec<SKaupatStore>,
}

#[derive(Deserialize)]
struct SKaupatStore {
    id: String,
    name: String,
    location: StoreLocation,
}

#[derive(Deserialize)]
struct StoreLocation {
    address: StoreAddress,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct StoreAddress {
    postcode_name: LocalizedString,
}

#[derive(Deserialize)]
struct LocalizedString {
    default: String,
}

#[derive(Debug, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct SKaupatStoreSearchView {
    pub total_hits: u64,
    pub results: Vec<SKaupatStoreView>,
}

#[derive(Debug, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct SKaupatStoreView {
    pub store_id: String,
    pub name: String,
    pub city: String,
}

impl From<SKaupatStore> for SKaupatStoreView {
    fn from(store: SKaupatStore) -> Self {
        Self {
            store_id: store.id,
            name: store.name,
            city: store.location.address.postcode_name.default,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_persisted_query_hash_from_network_url() {
        let extensions = serde_json::json!({
            "persistedQuery": { "version": 1, "sha256Hash": "abc123" }
        });
        let mut url = Url::parse(API_URL).unwrap();
        url.query_pairs_mut()
            .append_pair("operationName", "RemoteFilteredProducts")
            .append_pair("variables", "{}")
            .append_pair("extensions", &extensions.to_string());

        assert_eq!(
            hash_from_request_url(url.as_str(), "RemoteFilteredProducts").unwrap(),
            Some("abc123".to_string())
        );
        assert_eq!(
            hash_from_request_url(url.as_str(), "RemoteStoreSearch").unwrap(),
            None
        );
    }

    #[test]
    fn detects_persisted_query_not_found() {
        let missing = serde_json::json!({
            "errors": [{ "extensions": { "code": "PERSISTED_QUERY_NOT_FOUND" } }]
        });
        assert!(persisted_query_not_found(&missing));
        assert!(!persisted_query_not_found(&serde_json::json!({"data": {}})));
    }
}
