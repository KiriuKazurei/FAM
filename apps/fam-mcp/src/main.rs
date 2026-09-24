//! `fam-mcp`: the MCP server, as a stdio process with one credential.
//!
//! The whole executable is a transport: read a line from stdin, hand it to [`Server::handle`], write
//! the answer to stdout. The protocol, the gate and the fourteen tools live in `fam-svc-mcp` so they
//! can be tested without a pipe; what is tested here is the pipe.
//!
//! Two rules the transport exists to keep:
//!
//! * **stdout is protocol only.** Every diagnostic goes to stderr. A `log` line written to stdout
//!   would be read by the client as a JSON-RPC message, and the failure would look like a corrupt
//!   server rather than a chatty one.
//! * **The credential comes from the environment, never from a default.** `FAM_MCP_TOKEN` must be
//!   set and must verify, or the process exits before it reads a byte of stdin. A token in the
//!   command line would be visible to every other process on the machine that can list them, which
//!   is why there is no `--token` flag to add later "for convenience".

use std::io::{self, BufRead, Write};
use std::path::PathBuf;
use std::process::ExitCode;

use fam_core::Config;
use fam_svc_mcp::Server;

/// The environment variable the credential is read from.
const TOKEN_ENV: &str = "FAM_MCP_TOKEN";

fn main() -> ExitCode {
    // stderr before anything else can fail, so a refusal to start is something the person launching
    // this can see rather than a client's "server closed unexpectedly".
    let mut args = std::env::args().skip(1);
    let mut config_path: Option<PathBuf> = None;
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--config" => match args.next() {
                Some(path) => config_path = Some(path.into()),
                None => return fail("--config needs a path"),
            },
            "--help" | "-h" => {
                eprintln!(
                    "usage: fam-mcp [--config <path>]\n\nReads MCP messages, one per line, on stdin and \
                     answers on stdout. The credential comes from {TOKEN_ENV}; without it this process \
                     does not start. Logs go to stderr."
                );
                return ExitCode::SUCCESS;
            }
            other => return fail(format!("unrecognised argument {other:?}")),
        }
    }

    let credential = match std::env::var(TOKEN_ENV) {
        Ok(credential) if !credential.trim().is_empty() => credential,
        _ => {
            return fail(format!(
                "{TOKEN_ENV} is not set; run `fam mcp token add` and pass the \
                                 credential through the environment"
            ))
        }
    };

    let config = match &config_path {
        // Named on the command line: strict, because a typo in a path the operator chose should stop
        // the server rather than let it serve with defaults nobody asked for.
        Some(path) => match Config::load_from(path) {
            Ok(config) => config,
            Err(error) => return fail(format!("cannot read the configuration: {error}")),
        },
        // Not named: the platform file, with the same lenient fallback the CLI uses.
        None => Config::load(),
    };
    let server = match Server::authenticate(config, &credential) {
        Ok(server) => server,
        Err(error) => return fail(format!("the credential was not accepted: {error}")),
    };
    let label = server.token().label.clone();
    eprintln!(
        "fam-mcp: serving as agent:{label}; {} roots, {} tools",
        server.config().search_roots.len(),
        fam_svc_mcp::TOOLS.len()
    );

    let stdin = io::stdin();
    let mut output = io::stdout();
    let mut lines = stdin.lock().lines();
    let mut server = server;
    loop {
        let Some(line) = lines.next() else {
            break; // EOF: the client is gone.
        };
        let line = match line {
            Ok(line) => line,
            Err(source) => {
                eprintln!("fam-mcp: stdin failed: {source}; stopping");
                break;
            }
        };
        if let Some(answer) = server.handle(&line) {
            if let Err(source) = writeln!(output, "{answer}") {
                eprintln!("fam-mcp: stdout failed: {source}; stopping");
                return ExitCode::FAILURE;
            }
            // Flushed per message: a client that is waiting for an answer must not wait for the
            // buffer to fill.
            let _ = output.flush();
        }
    }
    ExitCode::SUCCESS
}

fn fail(message: impl AsRef<str>) -> ExitCode {
    eprintln!("fam-mcp: {}", message.as_ref());
    ExitCode::FAILURE
}
