use axum::{
    http::StatusCode,
    response::{IntoResponse, Response},
    Json,
};
use serde_json::json;

pub struct ApiErr(pub StatusCode, pub String);

pub fn internal(e: String) -> ApiErr {
    ApiErr(StatusCode::INTERNAL_SERVER_ERROR, e)
}

impl IntoResponse for ApiErr {
    fn into_response(self) -> Response {
        (self.0, Json(json!({ "error": self.1 }))).into_response()
    }
}
