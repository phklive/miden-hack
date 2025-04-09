use std::collections::HashMap;

use anyhow::Context;
use axum::{
    extract::{Query, State},
    http::StatusCode,
    response::IntoResponse,
    routing::{get, put},
};
use miden_client::{
    Client,
    account::{AccountBuilder, AccountStorageMode, AccountType, StorageSlot},
};
use miden_lib::transaction::TransactionKernel;
use miden_objects::account::{AccountComponent, StorageMap};
use rand::Rng;
use tower_http::cors::{Any, CorsLayer};

#[tokio::main]
async fn main() -> Result<(), anyhow::Error> {
    let subscriber = tracing_subscriber::fmt().finish();
    // Deploy account.
    let client = client().await.context("failed to create client")?;
    deploy_account(&client)
        .await
        .context("failed to deploy contract")?;

    tracing::info!("Account deployed");

    serve().await.context("failed to serve")
}

async fn deploy_account(client: &Client) -> anyhow::Result<()> {
    let slots = vec![StorageSlot::Map(StorageMap::new())];
    let assembler = TransactionKernel::assembler();
    let code = include_str!("../../contract/mns.masm");
    let component =
        AccountComponent::compile(code, assembler, slots).context("failed to compile contract")?;

    dbg!(&component);
    let mut rng = rand::rng();
    let account = AccountBuilder::new(rng.random())
        .account_type(AccountType::RegularAccountImmutableCode)
        .storage_mode(AccountStorageMode::Private)
        .with_component(component)
        .build()
        .context("failed to build account")?;

    Ok(())
}

async fn client() -> anyhow::Result<Client> {
    use std::sync::Arc;

    use miden_client::{
        Client, Felt,
        crypto::RpoRandomCoin,
        keystore::FilesystemKeyStore,
        rpc::{Endpoint, TonicRpcClient},
        store::{Store, sqlite_store::SqliteStore},
    };
    use miden_objects::crypto::rand::FeltRng;
    use rand::{Rng, rngs::StdRng};

    // Create the SQLite store from the client configuration.
    let sqlite_store = SqliteStore::new("store.sqlite".try_into()?).await?;
    let store = Arc::new(sqlite_store);

    // Generate a random seed for the RpoRandomCoin.
    let mut rng = rand::rng();
    let coin_seed: [u64; 4] = rng.random();

    // Initialize the random coin using the generated seed.
    let rng = RpoRandomCoin::new(coin_seed.map(Felt::new));
    let keystore = FilesystemKeyStore::new("keys".try_into()?)?;

    // Instantiate the client using a Tonic RPC client
    let endpoint = Endpoint::testnet();
    let client: Client = Client::new(
        Arc::new(TonicRpcClient::new(&endpoint, 10_000)),
        Box::new(rng),
        store,
        Arc::new(keystore),
        false, // Set to true for debug mode, if needed.
    );

    Ok(client)
}

#[derive(Clone, Copy)]
struct AppState;

async fn serve() -> anyhow::Result<()> {
    let app = axum::Router::new()
        .route("/register", put(register))
        .route("/lookup", get(lookup))
        .with_state(AppState)
        .layer(
            CorsLayer::new()
                .allow_origin(Any)
                .allow_methods(Any)
                .allow_headers(Any),
        );

    let listener = tokio::net::TcpListener::bind("0.0.0.0:3001")
        .await
        .context("failed to bind address")?;
    axum::serve(listener, app).await.context("failed to serve")
}

struct AppError(anyhow::Error);

async fn lookup(
    Query(params): Query<HashMap<String, String>>,
    State(state): State<AppState>,
) -> Result<(), AppError> {
    let name = params.get("name").context("missing `name` parameter")?;
    tracing::info!(name, "lookup called");

    Ok(())
}

async fn register(
    Query(params): Query<HashMap<String, String>>,
    State(state): State<AppState>,
) -> Result<(), AppError> {
    let name = params.get("name").context("missing `name` parameter")?;
    let id = params.get("id").context("missing `id` parameter")?;

    tracing::info!(name, id, "register called");

    Ok(())
}

impl From<anyhow::Error> for AppError {
    fn from(value: anyhow::Error) -> Self {
        Self(value)
    }
}

impl IntoResponse for AppError {
    fn into_response(self) -> axum::response::Response {
        let mut report = format!("{}", self.0);
        let mut err = self.0.source();
        while let Some(cause) = err {
            report.push_str(&format!("caused by: {cause}"));
            err = cause.source();
        }

        (StatusCode::INTERNAL_SERVER_ERROR, report).into_response()
    }
}
