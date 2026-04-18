//! Tera rendering — tiny wrapper so the rest of the extension doesn't care
//! about Tera's specific types. Render errors are converted to a String
//! message that the HTTP layer can log + surface.

use serde_json::Value;
use tera::{Context, Tera};

pub fn render(template: &str, data: &Value) -> Result<String, String> {
    let context = Context::from_value(data.clone())
        .map_err(|e| format!("tera: invalid template context: {e}"))?;
    Tera::one_off(template, &context, true)
        .map_err(|e| format!("tera: render failed: {e}"))
}
