//! Wire types. Kept apart from the engine types so the contract is explicit rather than
//! whatever `serde` would make of `fff_search`'s internals — and so the OpenAPI document
//! carries descriptions a generated client can read.

pub mod workspace;
