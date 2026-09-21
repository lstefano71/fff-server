//! A JSON extractor that fails as `problem+json`.
//!
//! `axum::Json`'s own rejection is `text/plain`, which would leave one class of error — a
//! malformed request body, the most likely one during client development — outside the
//! contract. A client deserialising `Problem` everywhere else would get a surprise exactly
//! where a clear message matters most.

use axum::extract::{FromRequest, Request};
use serde::de::DeserializeOwned;

use crate::error::ApiError;

pub struct Json<T>(pub T);

impl<T, S> FromRequest<S> for Json<T>
where
    T: DeserializeOwned,
    S: Send + Sync,
{
    type Rejection = ApiError;

    async fn from_request(req: Request, state: &S) -> Result<Self, Self::Rejection> {
        match axum::Json::<T>::from_request(req, state).await {
            Ok(axum::Json(value)) => Ok(Self(value)),
            // body_text() is the readable form, and it names the offending field plus the
            // fields that were expected, which is worth passing through verbatim.
            Err(rejection) => Err(ApiError::InvalidBody(rejection.body_text())),
        }
    }
}
