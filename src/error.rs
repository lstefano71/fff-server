use axum::http::{header, StatusCode};
use axum::response::{IntoResponse, Response};
use serde::Serialize;
use utoipa::ToSchema;

/// RFC 9457 problem details. ASP.NET Core deserialises this into `ProblemDetails` with no
/// client-side code.
///
/// Clients switch on `code`, never on `detail`: `code` is a stable identifier, `detail` is
/// prose from the engine and may change between fff releases.
#[derive(Debug, Serialize, ToSchema)]
pub struct Problem {
    /// Stable URN identifying the error class.
    #[serde(rename = "type")]
    #[schema(rename = "type", example = "urn:fff-server:error:invalid-path")]
    pub problem_type: String,
    /// Short, human-readable summary of the error class.
    pub title: String,
    /// HTTP status code, repeated here as RFC 9457 specifies.
    pub status: u16,
    /// Stable machine-readable code. Switch on this.
    #[schema(example = "invalid-path")]
    pub code: String,
    /// Prose explanation for this occurrence. Not stable; do not parse.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

/// Every failure this server reports. `Internal` is the documented catch-all that keeps a
/// future `fff_search::Error` variant from leaking as a debug string.
#[derive(Debug, thiserror::Error)]
pub enum ApiError {
    #[error("{0}")]
    InvalidPath(String),
    #[error("{0}")]
    InvalidGlob(String),
    #[error("{0}")]
    Forbidden(String),
    #[error("{0}")]
    NotFound(String),
    #[error("{0}")]
    Conflict(String),
    #[error("{0}")]
    NotReady(String),
    #[error("{0}")]
    Internal(String),
}

impl ApiError {
    fn parts(&self) -> (StatusCode, &'static str, &'static str) {
        match self {
            Self::InvalidPath(_) => (StatusCode::BAD_REQUEST, "invalid-path", "Invalid path"),
            Self::InvalidGlob(_) => (
                StatusCode::BAD_REQUEST,
                "invalid-glob-pattern",
                "Invalid glob pattern",
            ),
            Self::Forbidden(_) => (StatusCode::FORBIDDEN, "forbidden-root", "Forbidden root"),
            Self::NotFound(_) => (StatusCode::NOT_FOUND, "not-found", "Not found"),
            Self::Conflict(_) => (StatusCode::CONFLICT, "database-in-use", "Database in use"),
            Self::NotReady(_) => (
                StatusCode::SERVICE_UNAVAILABLE,
                "index-not-ready",
                "Index not ready",
            ),
            Self::Internal(_) => (
                StatusCode::INTERNAL_SERVER_ERROR,
                "internal",
                "Internal error",
            ),
        }
    }

    pub fn problem(&self) -> Problem {
        let (status, code, title) = self.parts();
        Problem {
            problem_type: format!("urn:fff-server:error:{code}"),
            title: title.to_owned(),
            status: status.as_u16(),
            code: code.to_owned(),
            detail: Some(self.to_string()),
        }
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let (status, ..) = self.parts();
        let body = serde_json::to_vec(&self.problem()).unwrap_or_else(|_| {
            br#"{"type":"urn:fff-server:error:internal","title":"Internal error","status":500,"code":"internal"}"#.to_vec()
        });
        if status.is_server_error() {
            tracing::error!(error = %self, status = status.as_u16(), "request failed");
        } else {
            tracing::debug!(error = %self, status = status.as_u16(), "request rejected");
        }
        (
            status,
            [(header::CONTENT_TYPE, "application/problem+json")],
            body,
        )
            .into_response()
    }
}

/// `fff_search::Error` is `#[non_exhaustive]`, so known variants are mapped explicitly and
/// anything new degrades to a 500 rather than failing to compile.
impl From<fff_search::Error> for ApiError {
    fn from(err: fff_search::Error) -> Self {
        use fff_search::Error as E;
        let text = err.to_string();
        match err {
            E::InvalidPath(_) => Self::InvalidPath(text),
            E::FilesystemRoot(_) => Self::Forbidden(text),
            E::InvalidGlobPattern { .. } => Self::InvalidGlob(text),
            E::FilePickerMissing | E::WatcherNotReady | E::WatcherDisabled => Self::NotReady(text),
            E::DbInUse { .. } | E::EnvSpecMismatch { .. } => Self::Conflict(text),
            _ => Self::Internal(text),
        }
    }
}

pub type ApiResult<T> = Result<T, ApiError>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn problem_carries_a_stable_code_and_urn() {
        let p = ApiError::InvalidPath("no such directory: Q:\\nope".into()).problem();
        assert_eq!(p.status, 400);
        assert_eq!(p.code, "invalid-path");
        assert_eq!(p.problem_type, "urn:fff-server:error:invalid-path");
        assert!(p.detail.unwrap().contains("Q:\\nope"));
    }

    #[test]
    fn type_field_serialises_as_type() {
        let json = serde_json::to_string(&ApiError::NotFound("workspace".into()).problem()).unwrap();
        assert!(json.contains(r#""type":"urn:fff-server:error:not-found""#));
        assert!(!json.contains("problemType"));
    }

    #[test]
    fn engine_filesystem_root_is_forbidden_not_a_bad_request() {
        let err: ApiError = fff_search::Error::FilesystemRoot("C:\\".into()).into();
        assert_eq!(err.problem().status, 403);
    }

    #[test]
    fn engine_picker_missing_is_retryable() {
        let err: ApiError = fff_search::Error::FilePickerMissing.into();
        assert_eq!(err.problem().status, 503);
        assert_eq!(err.problem().code, "index-not-ready");
    }
}
