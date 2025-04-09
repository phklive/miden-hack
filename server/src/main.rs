use std::collections::HashMap;

use anyhow::Context;
use axum::{
    extract::{Query, State},
    http::StatusCode,
    response::IntoResponse,
    routing::{get, put},
};

#[tokio::main]
async fn main() -> Result<(), anyhow::Error> {
    serve().await.context("failed to serve")
}

#[derive(Clone, Copy)]
struct AppState;

async fn serve() -> anyhow::Result<()> {
    let app = axum::Router::new()
        .route("register", put(register))
        .route("lookup", get(lookup))
        .with_state(AppState);

    let listener = tokio::net::TcpListener::bind("0.0.0.0:3000")
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
