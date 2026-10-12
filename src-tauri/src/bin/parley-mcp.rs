//! `parley-mcp`: MCP server (stdio) that gives Claude read-only access to
//! Parley's meetings and transcripts.
//!
//! Database location, in order: `--db <path>`, `$PARLEY_DB`, then the same
//! place the app uses (see `parley_lib::paths`).

use parley_lib::mcp;
use std::path::PathBuf;

const USAGE: &str = "parley-mcp — Parley meeting transcripts over MCP (stdio)

USAGE:
    parley-mcp [--db <path/to/parley.db>]

Register with Claude Code:
    claude mcp add parley -- /Applications/Parley.app/Contents/MacOS/parley-mcp

OPTIONS:
    --db <path>   Use this database instead of the app's (or set PARLEY_DB)
    --print-db    Print the database path that would be used and exit
    -h, --help    Show this help
    -V, --version Show the version";

fn main() {
    let mut args = std::env::args().skip(1);
    let mut db: Option<PathBuf> = None;
    let mut print_db = false;
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "-h" | "--help" => {
                println!("{USAGE}");
                return;
            }
            "-V" | "--version" => {
                println!("parley-mcp {}", env!("CARGO_PKG_VERSION"));
                return;
            }
            "--print-db" => print_db = true,
            "--db" => match args.next() {
                Some(path) => db = Some(PathBuf::from(path)),
                None => {
                    eprintln!("--db needs a path\n\n{USAGE}");
                    std::process::exit(2);
                }
            },
            other => {
                eprintln!("Unknown argument: {other}\n\n{USAGE}");
                std::process::exit(2);
            }
        }
    }
    let db = db.or_else(|| {
        std::env::var_os("PARLEY_DB")
            .filter(|v| !v.is_empty())
            .map(PathBuf::from)
    });
    let server = match db {
        Some(path) => mcp::Server::new(path),
        None => mcp::Server::following_app_settings(),
    };
    if print_db {
        println!("{}", server.db_path().display());
        return;
    }
    // stdout carries the protocol; diagnostics go to stderr.
    eprintln!("parley-mcp: serving {}", server.db_path().display());
    let stdin = std::io::stdin();
    let stdout = std::io::stdout();
    if let Err(e) = server.run(stdin.lock(), stdout.lock()) {
        eprintln!("parley-mcp: {e}");
        std::process::exit(1);
    }
}
