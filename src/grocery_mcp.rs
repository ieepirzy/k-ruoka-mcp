//! Unified MCP surface for K-Ruoka, S-Kaupat and Alko.
//!
//! K-Ruoka keeps its deep account/cart integration. S-Kaupat and Alko are deliberately
//! read-only catalogue providers. All three live behind one MCP server so an agent can
//! compare stores without needing to connect to three separate tool servers.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use anyhow::Result;
use rmcp::handler::server::router::tool::ToolRouter;
use rmcp::handler::server::wrapper::{Json, Parameters};
use rmcp::model::{ContentBlock, Implementation, IntoContents, ServerCapabilities, ServerInfo};
use rmcp::{ServerHandler, ServiceExt, tool, tool_handler, tool_router, transport::stdio};

use crate::alko_mcp::{SearchAlkoProductsArg, SearchAlkoStoresArg};
use crate::browser::basket::Cart;
use crate::browser::catalog::Catalog;
use crate::browser::offers::Offers;
use crate::browser::session::{ApiError, default_profile_dir, default_store_path};
use crate::browser::{KrApi, LaunchMode, Session};
use crate::login_flow::{ChildLogin, LoginFlow, LoginProgress};
use crate::mcp::tools::{
    AddArg, AuthArg, AuthStatus, DefaultStoreStatus, RemoveArg, SearchProductsArg, SearchStoresArg,
    SetDefaultStoreArg, StartLoginArg, StoreArg, UpdateArg,
};
use crate::providers::alko::{AlkoClient, AlkoProductSearchView, AlkoStoreView};
use crate::providers::s_kaupat::{SKaupatClient, SKaupatProductSearchView, SKaupatStoreSearchView};
use crate::s_kaupat_mcp::{SearchSKaupatProductsArg, SearchSKaupatStoresArg};
use crate::types::{
    CartView, DEFAULT_UNIT, PersonalOffersView, ProductSearchView, StoreSearchView,
};

const DEFAULT_DEBUG_PORT: u16 = 9222;

#[derive(Clone)]
pub struct GroceryServer {
    api: Arc<dyn KrApi>,
    login: Arc<dyn LoginFlow>,
    default_store: Arc<Mutex<Option<String>>>,
    store_path: Arc<PathBuf>,
    alko: Arc<AlkoClient>,
    s_kaupat: Arc<SKaupatClient>,
    tool_router: ToolRouter<Self>,
}

impl GroceryServer {
    pub fn from_session(
        session: Arc<Session>,
        login: Arc<ChildLogin>,
        store_path: PathBuf,
    ) -> Self {
        let initial_store = read_default_store(&store_path)
            .or_else(|| {
                std::env::var("K_RUOKA_DEFAULT_STORE")
                    .ok()
                    .map(|value| value.trim().to_string())
            })
            .filter(|value| !value.is_empty());
        let api: Arc<dyn KrApi> = session.clone();
        let login: Arc<dyn LoginFlow> = login;
        let s_kaupat = Arc::new(SKaupatClient::new(session));

        Self {
            api,
            login,
            default_store: Arc::new(Mutex::new(initial_store)),
            store_path: Arc::new(store_path),
            alko: Arc::new(AlkoClient::new()),
            s_kaupat,
            tool_router: Self::tool_router(),
        }
    }

    fn resolve_store(&self, provided: Option<String>) -> Result<String, GroceryToolFailure> {
        provided
            .or_else(|| self.default_store.lock().unwrap().clone())
            .ok_or_else(|| {
                GroceryToolFailure(
                    "No K-Ruoka store_id provided and no default store has been set. Call \
                     set_default_store first, or pass store_id explicitly."
                        .to_string(),
                )
            })
    }

    fn cart(&self) -> Cart<'_> {
        Cart::new(&*self.api)
    }

    fn catalog(&self) -> Catalog<'_> {
        Catalog::new(&*self.api)
    }

    fn offers(&self) -> Offers<'_> {
        Offers::new(&*self.api)
    }
}

pub struct GroceryToolFailure(String);

impl IntoContents for GroceryToolFailure {
    fn into_contents(self) -> Vec<ContentBlock> {
        vec![ContentBlock::text(self.0)]
    }
}

fn k_tool_failure(error: ApiError) -> GroceryToolFailure {
    GroceryToolFailure(match error {
        ApiError::AuthExpired => {
            "The K-Plussa session has expired. Run `k-ruoka-mcp login` on the machine \
             hosting this server, then retry."
                .to_string()
        }
        other => other.to_string(),
    })
}

fn read_default_store(path: &std::path::Path) -> Option<String> {
    std::fs::read_to_string(path)
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

async fn write_default_store(path: &std::path::Path, store_id: &str) {
    if let Some(parent) = path.parent()
        && let Err(error) = tokio::fs::create_dir_all(parent).await
    {
        eprintln!(
            "k-ruoka-mcp: could not create directory {}: {error}",
            parent.display()
        );
        return;
    }
    if let Err(error) = tokio::fs::write(path, store_id).await {
        eprintln!(
            "k-ruoka-mcp: could not save default store to {}: {error}",
            path.display()
        );
    }
}

#[tool_router]
impl GroceryServer {
    #[tool(
        annotations(
            title = "Search K-Ruoka products",
            read_only_hint = true,
            idempotent_hint = true
        ),
        description = "Search K-Ruoka products at a store. Results include store-specific \
                       pricing and availability. Uses the configured default K store when \
                       store_id is omitted."
    )]
    async fn search_products(
        &self,
        Parameters(arg): Parameters<SearchProductsArg>,
    ) -> Result<Json<ProductSearchView>, GroceryToolFailure> {
        let store_id = self.resolve_store(arg.store_id)?;
        self.catalog()
            .search_products(&store_id, &arg.query, arg.limit)
            .await
            .map(Into::into)
            .map(Json)
            .map_err(k_tool_failure)
    }

    #[tool(
        annotations(
            title = "Search K-Ruoka stores",
            read_only_hint = true,
            idempotent_hint = true
        ),
        description = "Find K-Ruoka stores by place or store name and return their store ids."
    )]
    async fn search_stores(
        &self,
        Parameters(arg): Parameters<SearchStoresArg>,
    ) -> Result<Json<StoreSearchView>, GroceryToolFailure> {
        self.catalog()
            .search_stores(&arg.query, arg.limit)
            .await
            .map(Into::into)
            .map(Json)
            .map_err(k_tool_failure)
    }

    #[tool(
        annotations(
            title = "Set default K-Ruoka store",
            read_only_hint = false,
            destructive_hint = false,
            idempotent_hint = true
        ),
        description = "Persist a default K-Ruoka store id so K tools can omit store_id."
    )]
    async fn set_default_store(
        &self,
        Parameters(SetDefaultStoreArg { store_id }): Parameters<SetDefaultStoreArg>,
    ) -> Result<Json<DefaultStoreStatus>, GroceryToolFailure> {
        *self.default_store.lock().unwrap() = Some(store_id.clone());
        write_default_store(&self.store_path, &store_id).await;
        Ok(Json(DefaultStoreStatus {
            default_store: store_id,
        }))
    }

    #[tool(
        annotations(
            title = "K-Ruoka personal offers",
            read_only_hint = true,
            idempotent_hint = true
        ),
        description = "Return the signed-in account's current OmaPlussa offers at a K-Ruoka \
                       store. Read-only. An anonymous session returns an empty list."
    )]
    async fn get_personal_offers(
        &self,
        Parameters(arg): Parameters<StoreArg>,
    ) -> Result<Json<PersonalOffersView>, GroceryToolFailure> {
        let store_id = self.resolve_store(arg.store_id)?;
        let offers = self
            .offers()
            .personal_offers(&store_id)
            .await
            .map_err(k_tool_failure)?;
        Ok(Json(PersonalOffersView {
            store_id,
            offers: offers.offers.into_iter().map(Into::into).collect(),
        }))
    }

    #[tool(
        annotations(
            title = "Read K-Ruoka cart",
            read_only_hint = true,
            idempotent_hint = true
        ),
        description = "Read the current K-Ruoka cart. This is also how to get basket itemId \
                       values for update_cart_item and remove_from_cart."
    )]
    async fn get_cart(
        &self,
        Parameters(StoreArg { store_id }): Parameters<StoreArg>,
    ) -> Result<Json<CartView>, GroceryToolFailure> {
        let store_id = self.resolve_store(store_id)?;
        self.cart()
            .active(&store_id)
            .await
            .map(Into::into)
            .map(Json)
            .map_err(k_tool_failure)
    }

    #[tool(
        annotations(
            title = "Add to K-Ruoka cart",
            destructive_hint = false,
            idempotent_hint = true
        ),
        description = "Set a product's quantity in the K-Ruoka cart by EAN. Search K-Ruoka \
                       products first when you need the EAN."
    )]
    async fn add_to_cart(
        &self,
        Parameters(arg): Parameters<AddArg>,
    ) -> Result<Json<CartView>, GroceryToolFailure> {
        let store_id = self.resolve_store(arg.store_id)?;
        self.cart()
            .add(
                &store_id,
                &arg.ean,
                arg.quantity.unwrap_or(1.0),
                arg.unit.as_deref().unwrap_or(DEFAULT_UNIT),
                arg.local_store_id,
                arg.allow_substitutes.unwrap_or(true),
            )
            .await
            .map(Into::into)
            .map(Json)
            .map_err(k_tool_failure)
    }

    #[tool(
        annotations(title = "Change K-Ruoka cart quantity", idempotent_hint = true),
        description = "Set the quantity of an existing K-Ruoka cart item. Takes basket itemId, \
                       not EAN. Quantity 0 removes the item."
    )]
    async fn update_cart_item(
        &self,
        Parameters(arg): Parameters<UpdateArg>,
    ) -> Result<Json<CartView>, GroceryToolFailure> {
        let store_id = self.resolve_store(arg.store_id)?;
        self.cart()
            .set_amount(&store_id, &arg.item_id, arg.quantity, arg.unit.as_deref())
            .await
            .map(Into::into)
            .map(Json)
            .map_err(k_tool_failure)
    }

    #[tool(
        annotations(title = "Remove from K-Ruoka cart", idempotent_hint = true),
        description = "Remove one K-Ruoka cart item by basket itemId."
    )]
    async fn remove_from_cart(
        &self,
        Parameters(arg): Parameters<RemoveArg>,
    ) -> Result<Json<CartView>, GroceryToolFailure> {
        let store_id = self.resolve_store(arg.store_id)?;
        self.cart()
            .remove(&store_id, &arg.item_id)
            .await
            .map(Into::into)
            .map(Json)
            .map_err(k_tool_failure)
    }

    #[tool(
        annotations(
            title = "Empty K-Ruoka cart",
            destructive_hint = true,
            idempotent_hint = true
        ),
        description = "Remove every item from the K-Ruoka cart. Confirm with the user first."
    )]
    async fn clear_cart(
        &self,
        Parameters(StoreArg { store_id }): Parameters<StoreArg>,
    ) -> Result<Json<CartView>, GroceryToolFailure> {
        let store_id = self.resolve_store(store_id)?;
        self.cart()
            .clear(&store_id)
            .await
            .map(Into::into)
            .map(Json)
            .map_err(k_tool_failure)
    }

    #[tool(
        annotations(
            title = "Check K-Plussa sign-in",
            read_only_hint = true,
            idempotent_hint = true
        ),
        description = "Check whether the stored K-Plussa browser session is signed in."
    )]
    async fn auth_status(
        &self,
        Parameters(AuthArg { store_id }): Parameters<AuthArg>,
    ) -> Result<Json<AuthStatus>, GroceryToolFailure> {
        let store_id = store_id
            .or_else(|| self.default_store.lock().unwrap().clone())
            .unwrap_or_else(|| crate::login::DEFAULT_PROBE_STORE.to_string());
        match self.cart().active(&store_id).await {
            Ok(basket) => {
                let account = basket.user_info.display();
                Ok(Json(match &account {
                    Some(who) => AuthStatus {
                        logged_in: true,
                        account: Some(who.clone()),
                        detail: format!("Signed in as {who}."),
                    },
                    None => AuthStatus {
                        logged_in: false,
                        account: None,
                        detail: "Not signed in. The reachable cart is anonymous, not the \
                                 account's. Start login or run `k-ruoka-mcp login`."
                            .to_string(),
                    },
                }))
            }
            Err(ApiError::AuthExpired) => Ok(Json(AuthStatus {
                logged_in: false,
                account: None,
                detail: "The K-Plussa session has expired. Start login again.".to_string(),
            })),
            Err(error) => Err(k_tool_failure(error)),
        }
    }

    #[tool(
        annotations(
            title = "Start K-Plussa login",
            read_only_hint = false,
            idempotent_hint = true
        ),
        description = "Open the K-Plussa login flow. Relay the returned instructions to the \
                       user verbatim, then poll login_status."
    )]
    async fn start_login(
        &self,
        Parameters(arg): Parameters<StartLoginArg>,
    ) -> Result<Json<LoginProgress>, GroceryToolFailure> {
        self.login
            .start(arg.port.unwrap_or(DEFAULT_DEBUG_PORT))
            .await
            .map(Json)
            .map_err(k_tool_failure)
    }

    #[tool(
        annotations(
            title = "K-Plussa login status",
            read_only_hint = true,
            idempotent_hint = true
        ),
        description = "Return the status of a login started by start_login."
    )]
    async fn login_status(&self) -> Result<Json<LoginProgress>, GroceryToolFailure> {
        self.login.status().await.map(Json).map_err(k_tool_failure)
    }

    #[tool(
        annotations(title = "Cancel K-Plussa login", idempotent_hint = true),
        description = "Cancel an in-progress K-Plussa login and restore normal browser access."
    )]
    async fn cancel_login(&self) -> Result<Json<LoginProgress>, GroceryToolFailure> {
        self.login.cancel().await.map(Json).map_err(k_tool_failure)
    }

    #[tool(
        annotations(
            title = "Search S-Kaupat products",
            read_only_hint = true,
            idempotent_hint = true
        ),
        description = "Search a specific S-Kaupat store's catalogue. Prices and assortment \
                       are store-specific."
    )]
    async fn search_s_kaupat_products(
        &self,
        Parameters(arg): Parameters<SearchSKaupatProductsArg>,
    ) -> Result<Json<SKaupatProductSearchView>, GroceryToolFailure> {
        self.s_kaupat
            .search_products(&arg.query, &arg.store_id, arg.limit)
            .await
            .map(Json)
            .map_err(|error| GroceryToolFailure(error.to_string()))
    }

    #[tool(
        annotations(
            title = "Search S-Kaupat stores",
            read_only_hint = true,
            idempotent_hint = true
        ),
        description = "Search S-Kaupat stores by place or store name and return store ids."
    )]
    async fn search_s_kaupat_stores(
        &self,
        Parameters(arg): Parameters<SearchSKaupatStoresArg>,
    ) -> Result<Json<SKaupatStoreSearchView>, GroceryToolFailure> {
        self.s_kaupat
            .search_stores(arg.query.as_deref())
            .await
            .map(Json)
            .map_err(|error| GroceryToolFailure(error.to_string()))
    }

    #[tool(
        annotations(
            title = "Search Alko products",
            read_only_hint = true,
            idempotent_hint = true
        ),
        description = "Search Alko's catalogue nationally or at one store. Read-only."
    )]
    async fn search_alko_products(
        &self,
        Parameters(arg): Parameters<SearchAlkoProductsArg>,
    ) -> Result<Json<AlkoProductSearchView>, GroceryToolFailure> {
        self.alko
            .search_products(&arg.query, arg.store_id.as_deref(), arg.limit)
            .await
            .map(Json)
            .map_err(|error| GroceryToolFailure(error.to_string()))
    }

    #[tool(
        annotations(
            title = "Search Alko stores",
            read_only_hint = true,
            idempotent_hint = true
        ),
        description = "List Alko stores, optionally filtered by city. Read-only."
    )]
    async fn search_alko_stores(
        &self,
        Parameters(arg): Parameters<SearchAlkoStoresArg>,
    ) -> Result<Json<Vec<AlkoStoreView>>, GroceryToolFailure> {
        self.alko
            .stores(arg.city.as_deref())
            .await
            .map(Json)
            .map_err(|error| GroceryToolFailure(error.to_string()))
    }
}

#[tool_handler(router = self.tool_router)]
impl ServerHandler for GroceryServer {
    fn get_info(&self) -> ServerInfo {
        ServerInfo::new(ServerCapabilities::builder().enable_tools().build())
            .with_server_info(Implementation::new(
                "finland-grocery-mcp",
                env!("CARGO_PKG_VERSION"),
            ))
            .with_instructions(
                "Finnish grocery tools covering K-Ruoka, S-Kaupat and Alko. K-Ruoka has \
                 account/cart/OmaPlussa support; S-Kaupat and Alko are read-only catalogue \
                 providers. Use store search before store-scoped product search. Never place \
                 an order or spend money: checkout is intentionally out of scope.",
            )
    }
}

pub async fn serve() -> Result<()> {
    let profile_dir = default_profile_dir()?;
    let store_path = default_store_path(&profile_dir);
    let session = Arc::new(Session::new(profile_dir, LaunchMode::Headless)?);
    let login = Arc::new(ChildLogin::new(Arc::clone(&session)));
    let handler = GroceryServer::from_session(Arc::clone(&session), Arc::clone(&login), store_path);

    let service = handler.serve(stdio()).await?;
    let outcome = service.waiting().await;

    session.signal_shutdown();
    login.shutdown().await;
    session.close().await.ok();
    outcome?;
    Ok(())
}
