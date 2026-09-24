//! The MCP face of FAM: a credential-checked, scope-checked, audited tool server.
//!
//! This crate is the protocol and the tools; [`apps/fam-mcp`](../../apps/fam-mcp) is the executable
//! that wires it to stdin and stdout. The split is the same one every other layer in this workspace
//! makes - logic that can be unit tested, and a transport that can be run.
//!
//! Three things are true of every request, in this order, and the order is the security property:
//!
//! 1. the credential's **tier** decides whether the verb exists for this caller at all;
//! 2. the credential's **workspaces** decide whether the path is reachable, by path identity and
//!    after the filesystem resolves links - see [`gate`];
//! 3. one **audit line** per call, written whatever the outcome, including the refusals.
//!
//! There is no fifth tool that manages credentials and no sixth that reads the trail. The MCP surface
//! advertises fourteen tools and every one of them is a question about files; the credential commands
//! exist only in `fam mcp token`, which is a program a person runs. That is not enforced by checking
//! a name at runtime - `fam-svc-auth` is reachable from here only to *verify* a token, and the
//! mutation methods (`add`, `revoke`, `rotate`) are never called on this path. The test that keeps it
//! true is `tests/mcp.rs::the_advertised_tools_are_the_files_and_nothing_else`, which asserts the
//! list by name and asserts the absence of every credential and audit word in it.
//!
//! # What this cannot promise
//!
//! An agent that can run arbitrary programs as this user does not need this server: it can read the
//! same files directly, with the same permissions, and no line would be written. This layer is a
//! barrier on the surface it fronts - what an agent asks *this program* to do - and closing the
//! other case needs OS-level isolation, which is out of scope by decision, not by omission.
//!
//! The same applies to a running server and a revoked credential: revocation stops new connections
//! immediately, and this one refuses further calls once the credential file changes under it rather
//! than serving on a table it may no longer agree with. Restarting is how an operator stops a server
//! anyway.

mod gate;
mod mutations;
mod proto;
mod server;
mod tools;

pub use gate::{Gate, Refusal};
pub use proto::{
    parse_line, Id, ProtocolError, Request, Response, INTERNAL_ERROR, INVALID_PARAMS,
    INVALID_REQUEST, METHOD_NOT_FOUND, PARSE_ERROR,
};
pub use server::{Server, PROTOCOL_VERSION, SERVER_NAME};
pub use tools::{find, spec, ToolSpec, TOOLS};

use std::path::PathBuf;

/// Why the server could not be built or could not run a call. Everything here is a reason the
/// process refuses to serve, not a tool result - a tool result is a JSON payload, this is a startup
/// or plumbing failure.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error(
        "the credential was not accepted: it is unknown, its secret is wrong, or it is revoked"
    )]
    NotAuthorized,
    #[error("cannot read the credential file: {0}")]
    Credentials(#[from] fam_svc_auth::Error),
    #[error("cannot open the audit log: {0}")]
    Audit(#[from] fam_svc_audit::Error),
    #[error("cannot open the tag store: {0}")]
    Store(#[from] fam_svc_store::StoreError),
    #[error("cannot read {}: {reason}", path.display())]
    Io { path: PathBuf, reason: String },
    #[error("the request was not a valid call: {0}")]
    Protocol(String),
}
