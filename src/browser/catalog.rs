//! Catalogue lookups: finding a product's EAN, and finding a store's id.
//!
//! Separate from `basket` because none of this touches a cart. Both endpoints are
//! read-only, and both exist because the cart tools take opaque identifiers that a
//! caller otherwise has no way to obtain.

use crate::browser::session::{ApiError, KrApi};
use crate::types::{ProductSearchResponse, StoreSearchResponse};

/// Keeps one response from being large enough to swamp a model's context. The API's own
/// default is 48; the tools cap at this.
const MAX_LIMIT: u32 = 50;
const DEFAULT_LIMIT: u32 = 10;

pub struct Catalog<'a> {
    api: &'a dyn KrApi,
}

impl<'a> Catalog<'a> {
    pub fn new(api: &'a dyn KrApi) -> Self {
        Self { api }
    }

    /// Search products at a store.
    ///
    /// The term goes in the *path*, percent-encoded, with paging and store in the query
    /// string. Results are store-scoped: price and availability differ per store, so the
    /// same term at a different store is a genuinely different answer.
    pub async fn search_products(
        &self,
        store_id: &str,
        query: &str,
        limit: Option<u32>,
    ) -> Result<ProductSearchResponse, ApiError> {
        let query = query.trim();
        if query.is_empty() {
            return Err(ApiError::InvalidRequest(
                "query must not be empty. Pass something to search for, e.g. \"banaani\"."
                    .to_string(),
            ));
        }
        let limit = clamp_limit(limit);
        let path = format!(
            "/kr-api/v2/product-search/{}?language=fi&storeId={}&offset=0&limit={limit}",
            percent_encode(query),
            percent_encode(store_id),
        );
        let value = self.api.call("POST", &path, None).await?;
        serde_json::from_value(value)
            .map_err(|e| ApiError::Other(anyhow::anyhow!("unexpected product-search shape: {e}")))
    }

    /// Search stores by name or place.
    ///
    /// Unlike product search, the term goes in a JSON body.
    pub async fn search_stores(
        &self,
        query: &str,
        limit: Option<u32>,
    ) -> Result<StoreSearchResponse, ApiError> {
        let query = query.trim();
        if query.is_empty() {
            return Err(ApiError::InvalidRequest(
                "query must not be empty. Pass a place or store name, e.g. \"Ruoholahti\"."
                    .to_string(),
            ));
        }
        let body = serde_json::json!({
            "query": query,
            "limit": clamp_limit(limit),
            "offset": 0,
        });
        let value = self
            .api
            .call("POST", "/kr-api/stores/search", Some(&body))
            .await?;
        serde_json::from_value(value)
            .map_err(|e| ApiError::Other(anyhow::anyhow!("unexpected stores/search shape: {e}")))
    }
}

fn clamp_limit(limit: Option<u32>) -> u32 {
    limit.unwrap_or(DEFAULT_LIMIT).clamp(1, MAX_LIMIT)
}

/// Percent-encode a path segment or query value.
///
/// Hand-rolled rather than pulling in a dependency for it: the allowed set here is
/// deliberately conservative (unreserved characters only), so anything else -- spaces,
/// Finnish letters, `?`, `&`, `#`, `/` -- is escaped rather than being able to change
/// which URL is requested.
fn percent_encode(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for byte in value.as_bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(*byte as char);
            }
            other => out.push_str(&format!("%{other:02X}")),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn limits_are_clamped_into_range() {
        assert_eq!(clamp_limit(None), DEFAULT_LIMIT);
        assert_eq!(clamp_limit(Some(0)), 1);
        assert_eq!(clamp_limit(Some(5)), 5);
        assert_eq!(clamp_limit(Some(9999)), MAX_LIMIT);
    }

    /// The search term reaches the server inside a URL, so anything that could end the
    /// path or start a new parameter has to be escaped. `&limit=` here would otherwise
    /// override the real one.
    #[test]
    fn encoding_escapes_everything_that_could_change_the_url() {
        assert_eq!(percent_encode("banaani"), "banaani");
        assert_eq!(percent_encode("maito 1l"), "maito%201l");
        assert_eq!(percent_encode("a&limit=9999"), "a%26limit%3D9999");
        assert_eq!(percent_encode("a/../b"), "a%2F..%2Fb");
        assert_eq!(percent_encode("a?b#c"), "a%3Fb%23c");
        // Finnish letters are multi-byte UTF-8 and must be escaped per byte.
        assert_eq!(percent_encode("ä"), "%C3%A4");
    }
}
