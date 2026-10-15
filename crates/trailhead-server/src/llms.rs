//! `GET /llms.txt`: the API in one page of markdown, for agents building a client.

use axum::http::header;
use axum::response::IntoResponse;

pub const TEXT: &str = include_str!("llms.txt");

pub async fn serve() -> impl IntoResponse {
    ([(header::CONTENT_TYPE, "text/plain; charset=utf-8")], TEXT)
}

#[cfg(test)]
mod tests {
    use axum::response::IntoResponse;

    use super::TEXT;
    use crate::error::ApiError;
    use crate::openapi::ERROR_CODES;
    use crate::openapi::tests::sample_errors;

    #[test]
    fn documents_every_error_code_with_its_status() {
        let errors = sample_errors();
        assert_eq!(errors.len(), ERROR_CODES.len());
        for e in errors {
            let code = e.code();
            let status = ApiError(e).into_response().status().as_u16();
            let line = format!("- `{code}` {status}: ");
            assert!(TEXT.contains(&line), "llms.txt is missing {line:?}");
        }
    }
}
