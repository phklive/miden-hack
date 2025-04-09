use std::{collections::HashMap, time::Duration};

use anyhow::Context;
use axum::{
    extract::{Query, State},
    http::StatusCode,
    response::IntoResponse,
    routing::{get, put},
};
use miden_client::{
    Client, Word,
    account::{Account, AccountBuilder, AccountStorageMode, AccountType, StorageSlot},
    transaction::{
        LocalTransactionProver, TransactionRequest, TransactionRequestBuilder, TransactionScript,
    },
};
use miden_lib::transaction::TransactionKernel;
use miden_objects::{
    account::{AccountComponent, StorageMap},
    vm::Program,
};
use rand::Rng;
use tokio::sync::{mpsc, oneshot};
use tower_http::cors::{Any, CorsLayer};

#[tokio::main]
async fn main() -> Result<(), anyhow::Error> {
    tracing_subscriber::fmt().init();
    tracing::info!("Spinning up");
    // Deploy account.
    let mut client = client().await.context("failed to create client")?;
    let account = deploy_account(&mut client)
        .await
        .context("failed to deploy contract")?;

    tracing::info!(id=%account.id(), "Account deployed");

    serve().await.context("failed to serve")
}

async fn deploy_account(client: &mut Client) -> anyhow::Result<Account> {
    let slots = vec![StorageSlot::Map(StorageMap::new())];
    let assembler = TransactionKernel::assembler();
    let code = include_str!("../../contract/mns.masm");
    let component =
        AccountComponent::compile(code, assembler, slots).context("failed to compile contract")?;

    let mut rng = rand::rng();
    let (account, seed) = AccountBuilder::new(rng.random())
        .account_type(AccountType::RegularAccountImmutableCode)
        .storage_mode(AccountStorageMode::Private)
        .with_component(component)
        .build()
        .context("failed to build account")?;

    client
        .add_account(&account, seed.into(), false)
        .await
        .context("failed to add account")?;

    Ok(account)
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

#[derive(Clone)]
struct AppState {
    sender: mpsc::Sender<Command>,
}

async fn serve() -> anyhow::Result<()> {
    let (tx, rx) = mpsc::channel(1024);
    let app = axum::Router::new()
        .route("/register", put(register))
        .route("/lookup", get(lookup))
        .with_state(AppState { sender: tx })
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
) -> Result<String, AppError> {
    let name = params
        .get("name")
        .context("missing `name` parameter")?
        .clone();
    tracing::info!(name, "lookup called");

    let (tx, rx) = oneshot::channel();

    state
        .sender
        .send_timeout(Command::Lookup { name, ret: tx }, Duration::from_secs(5))
        .await
        .context("failed to send lookup command to client")?;

    let name = rx
        .await
        .context("command channel failed")?
        .context("lookup failed")?;

    Ok(name)
}

async fn register(
    Query(params): Query<HashMap<String, String>>,
    State(state): State<AppState>,
) -> Result<(), AppError> {
    let name = params
        .get("name")
        .context("missing `name` parameter")?
        .clone();
    let account = params.get("id").context("missing `id` parameter")?.clone();

    let (tx, rx) = oneshot::channel();

    tracing::info!(name, id=%account, "register called");

    state
        .sender
        .send_timeout(
            Command::Register {
                name,
                account,
                ret: tx,
            },
            Duration::from_secs(5),
        )
        .await
        .context("failed to send register command to client")?;

    rx.await
        .context("command channel failed")?
        .context("registration failed")?;

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

enum Command {
    Register {
        name: String,
        account: String,
        ret: oneshot::Sender<Result<(), anyhow::Error>>,
    },
    Lookup {
        name: String,
        ret: oneshot::Sender<Result<String, anyhow::Error>>,
    },
}

async fn run_client(
    mut client: Client,
    account: Account,
    mut cmds: mpsc::Receiver<Command>,
) -> anyhow::Result<()> {
    while let Some(cmd) = cmds.recv().await {
        match cmd {
            Command::Register { name, account, ret } => todo!(),
            Command::Lookup { name, ret } => {
                let code = r#"
                    start
                        push.0
                        drop
                    end
                 "#;
                let script = client.compile_tx_script([], &code).unwrap();
                let tx = TransactionRequestBuilder::new()
                    .with_custom_script(script)
                    .build()
                    .context("failed to build lookup tx")?;
                let result = client
                    .new_transaction(account.id(), tx)
                    .await
                    .context("lookup failed")?;
                println!("Result: {:?}", result)
            }
        }
    }

    Ok(())
}
