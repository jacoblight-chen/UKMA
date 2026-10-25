//! UKMA gateway: registry-driven MCP tool + REST dispatch for the 65 domain
//! operations and the 4 system operations.
//!
//! Normative source: `docs/UKMA_v1.2.3_Full_Spec.md` §31.
//!
//! Properties this crate enforces:
//! - the operation registry is the only source of tools, routes and error sets;
//!   the manifest is projected from it and drift is reported, never hand-written;
//! - the gateway authenticates and injects the trusted scope — client-declared
//!   scopes are expectations only (§31.4);
//! - deny-by-default authorization, and an invisible object is reported as
//!   `NOT_FOUND` so existence never leaks across tenants (§40.7);
//! - a handler can never emit a code its operation did not declare: undeclared
//!   codes are normalised to `INTERNAL` with a warning.

pub mod alias;
pub mod context;
pub mod error;
pub mod gateway;
pub mod handlers;
pub mod instant;
pub mod manifest;
pub mod rest;
pub mod sdk;

pub use context::{visibility, visibility_error, OpContext, Visibility};
pub use error::{DomainFailure, GatewayError};
pub use gateway::{Gateway, GatewayOutcome, Handler};
pub use handlers::{build, default_gateway, JobIndex, JobRecord, Services};
pub use manifest::{drift, manifest_json, tool_by_name, tools, Tool};
pub use rest::{resolve, Route};
pub use sdk::{ApiError, Client, ClientError, InProcess, Transport};
