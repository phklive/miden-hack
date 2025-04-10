use std::{collections::HashMap, sync::Arc, time::Duration};

use anyhow::Context;
use axum::{
    extract::{Query, State},
    http::StatusCode,
    response::IntoResponse,
    routing::{get, put},
};
use miden_client::{
    Client, Felt, Word,
    account::{Account, AccountBuilder, AccountStorageMode, AccountType, StorageSlot},
    transaction::{TransactionRequestBuilder, TransactionScript},
};
use miden_lib::{transaction::TransactionKernel, utils::PushMany};
use miden_objects::{
    account::{AccountComponent, StorageMap},
    assembly::{DefaultSourceManager, LibraryPath, Module},
};
use tokio::sync::{mpsc, oneshot};
use tower_http::cors::{Any, CorsLayer};

#[tokio::main]
async fn main() -> Result<(), anyhow::Error> {
    tracing_subscriber::fmt().init();
    tracing::info!("Spinning up");

    let (tx, rx) = mpsc::channel(100);
    let (t, r) = oneshot::channel();
    tx.send(Command::Register {
        name: "mirko".to_string(),
        account_id: "0x1234".to_string(),
        ret: t,
    })
    .await
    .unwrap();
    // let state = AppState { sender: tx };

    // tokio::spawn(serve(state));
    command_handler(rx)
        .await
        .context("failed to run client handler")
}

async fn deploy_account(client: &mut Client) -> anyhow::Result<Account> {
    let slots = vec![StorageSlot::Map(StorageMap::new())];
    let assembler = TransactionKernel::assembler();
    let code = include_str!("../../contract/mns.masm");
    let component = AccountComponent::compile(code, assembler, slots)
        .context("failed to compile contract")?
        .with_supported_type(AccountType::RegularAccountImmutableCode);

    let anchor_block = client.get_latest_epoch_block().await.unwrap();
    tracing::info!("anchor block");
    let mut seed: [u8; 32] = Default::default();
    seed[0] = 0x13;
    seed[12] = 0x44;
    let (account, seed) = AccountBuilder::new(seed)
        .account_type(AccountType::RegularAccountImmutableCode)
        .storage_mode(AccountStorageMode::Private)
        .anchor((&anchor_block).try_into().unwrap())
        .with_component(component)
        .build()
        .context("failed to build account")?;

    client
        .add_account(&account, seed.into(), true)
        .await
        .context("failed to add account")?;

    Ok(account)
}

async fn client() -> anyhow::Result<Client> {
    use miden_client::{
        Client, Felt,
        crypto::RpoRandomCoin,
        keystore::FilesystemKeyStore,
        rpc::{Endpoint, TonicRpcClient},
        store::sqlite_store::SqliteStore,
    };
    use rand::Rng;

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

async fn serve(state: AppState) -> anyhow::Result<()> {
    let app = axum::Router::new()
        .route("/register", put(register))
        .route("/lookup", get(lookup))
        .with_state(state)
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
        .map_err(|err| anyhow::anyhow!("lookup failed: {err}"))?;

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
                account_id: account,
                ret: tx,
            },
            Duration::from_secs(5),
        )
        .await
        .context("failed to send register command to client")?;

    rx.await
        .context("command channel failed")?
        .map_err(|err| anyhow::anyhow!("lookup failed: {err}"))?;

    Ok(())
}

impl From<anyhow::Error> for AppError {
    fn from(value: anyhow::Error) -> Self {
        Self(value)
    }
}

impl IntoResponse for AppError {
    fn into_response(self) -> axum::response::Response {
        (StatusCode::INTERNAL_SERVER_ERROR, format!("{}", self)).into_response()
    }
}

impl std::fmt::Display for AppError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&format!("{}", self.0));
        let mut err = self.0.source();
        while let Some(cause) = err {
            f.write_str(&format!("caused by: {cause}"));
            err = cause.source();
        }
        Ok(())
    }
}

enum Command {
    Register {
        name: String,
        account_id: String,
        ret: oneshot::Sender<Result<(), String>>,
    },
    Lookup {
        name: String,
        ret: oneshot::Sender<Result<String, String>>,
    },
}

async fn command_handler(mut cmds: mpsc::Receiver<Command>) -> anyhow::Result<()> {
    let mut client = client().await.context("failed to create client")?;
    let account = deploy_account(&mut client)
        .await
        .context("failed to deploy contract")?;

    let source_manager = Arc::new(DefaultSourceManager::default());
    let lib = Module::parser(miden_objects::assembly::ModuleKind::Library)
        .parse_str(
            LibraryPath::new("hacky::mns").unwrap(),
            include_str!("../../contract/mns.masm"),
            &source_manager,
        )
        .unwrap();
    let assembler = TransactionKernel::assembler().with_debug_mode(true);
    let lib = assembler.clone().assemble_library([lib]).unwrap();

    tracing::info!(id=%account.id(), "Account deployed");
    while let Some(cmd) = cmds.recv().await {
        match cmd {
            Command::Register {
                name,
                ret,
                account_id,
            } => {
                let result =
                    handle_register(&mut client, &account, &assembler, &lib, name, account_id)
                        .await
                        .map_err(|err| format!("{}", AppError::from(err)));

                ret.send(result).unwrap();
            }
            Command::Lookup { name, ret } => {
                let result = handle_lookup(&mut client, &account, &assembler, &lib, name)
                    .await
                    .map_err(|err| format!("{}", AppError::from(err)));
                ret.send(result).unwrap();
            }
        }
    }

    Ok(())
}

async fn handle_lookup(
    client: &mut Client,
    account: &Account,
    assembler: &miden_objects::assembly::Assembler,
    lib: &miden_objects::assembly::Library,
    name: String,
) -> Result<String, anyhow::Error> {
    let name = str_to_word(&name).context("failed to encode name as word")?;
    let code = format!(
        r#"
            use.hacky::mns
            use.std::sys
            
            begin
                push.{0}.{1}.{2}.{3}
                call.mns::lookup

                exec.sys::truncate_stack
            end
         "#,
        name[0], name[1], name[2], name[3]
    );
    let script = TransactionScript::compile(
        code,
        [],
        assembler.clone().with_library(lib.clone()).unwrap(),
    )
    .unwrap();
    let tx = TransactionRequestBuilder::new()
        .with_custom_script(script)
        .build()
        .context("failed to build lookup tx")?;
    let result = client
        .new_transaction(account.id(), tx)
        .await
        .context("lookup failed")?;
    Ok(println!("Result: {:?}", result))
}

async fn handle_register(
    client: &mut Client,
    account: &Account,
    assembler: &miden_objects::assembly::Assembler,
    lib: &miden_objects::assembly::Library,
    name: String,
    account_id: String,
) -> Result<(), anyhow::Error> {
    let name = str_to_word(&name).context("failed to encode name as word")?;
    let account_id = str_to_word(&account_id).context("failed to encode account ID as word")?;
    let code = format!(
        r#"
                    use.hacky::mns
                    use.std::sys
                    
                    begin
                        push.111 debug.stack drop
                    
                        push.{0}.{1}.{2}.{3}
                        push.{4}.{5}.{6}.{7}
                        call.mns::register

                        exec.sys::truncate_stack
                    end
                 "#,
        name[0],
        name[1],
        name[2],
        name[3],
        account_id[0],
        account_id[1],
        account_id[2],
        account_id[3]
    );
    let script = TransactionScript::compile(
        code,
        [],
        assembler.clone().with_library(lib.clone()).unwrap(),
    )
    .unwrap();
    client.sync_state().await.context("failed to sync state")?;
    let tx = TransactionRequestBuilder::new()
        .with_custom_script(script)
        .build()
        .context("failed to build register tx")?;
    let result = client
        .new_transaction(account.id(), tx)
        .await
        .context("register failed")?;
    Ok(())
}

fn str_to_word(s: &str) -> anyhow::Result<Word> {
    let bytes = s.as_bytes();
    anyhow::ensure!(
        bytes.len() <= 4 * 7,
        "string `{s}` is too large for word: {} bytes",
        bytes.len()
    );

    let mut word = Word::default();

    for (i, felt) in bytes.chunks(7).enumerate() {
        let mut felt = felt.to_vec();
        felt.push_many(0, 8 - felt.len());
        let felt = u64::from_be_bytes(felt.try_into().unwrap());
        let felt = Felt::new(felt);

        word[i] = felt;
    }

    Ok(word)
}

fn word_to_str(word: Word) -> String {
    let mut bytes = Vec::new();

    for felt in word {
        let felt = felt.inner().to_be_bytes();

        bytes.extend_from_slice(&felt[..7]);
    }

    String::from_utf8(bytes).unwrap()
}
