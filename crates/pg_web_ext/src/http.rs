//! Axum HTTP layer — the thin shell over Hyper.
//!
//! In M1.1 (walking skeleton) this just returns a literal for any request.
//! Step 3 replaces the fallback with the SPI → Tera render pipeline.

use axum::Router;

/// Build the root router.
pub fn app() -> Router {
    Router::new().fallback(hello)
}

/// Step-2 placeholder handler. Returns a plain-text greeting for any path/method.
async fn hello() -> &'static str {
    "hello from pg-web\n"
}
