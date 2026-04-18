//! Axum HTTP layer — thin shell. Delegates to `router::serve` for SPI + Tera.

use axum::{
    extract::Request,
    http::{header, StatusCode},
    response::{IntoResponse, Response},
    Router,
};
use tracing::error;

use crate::router::{self, ServeOutcome};

pub fn app() -> Router {
    Router::new().fallback(handle)
}

async fn handle(req: Request) -> Response {
    let method = req.method().as_str().to_string();
    let path = req.uri().path().to_string();

    match router::serve(&method, &path) {
        ServeOutcome::Html(body) => (
            StatusCode::OK,
            [(header::CONTENT_TYPE, "text/html; charset=utf-8")],
            body,
        )
            .into_response(),
        ServeOutcome::NotFound => (
            StatusCode::NOT_FOUND,
            [(header::CONTENT_TYPE, "text/plain; charset=utf-8")],
            "not found\n",
        )
            .into_response(),
        ServeOutcome::Error(err) => {
            error!(method = %method, path = %path, error = %err, "handler error");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                [(header::CONTENT_TYPE, "text/plain; charset=utf-8")],
                "internal server error\n",
            )
                .into_response()
        }
    }
}
