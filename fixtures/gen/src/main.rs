//! Writes every golden fixture chain as `<dir>/<name>.chain.json`.
//!
//! Usage: `cargo run -p gex-fixtures --bin gen -- fixtures/golden`

use std::path::PathBuf;
use std::process::ExitCode;

fn main() -> ExitCode {
    let Some(dir) = std::env::args_os().nth(1).map(PathBuf::from) else {
        eprintln!("usage: gen <output-dir>");
        return ExitCode::FAILURE;
    };
    if let Err(e) = std::fs::create_dir_all(&dir) {
        eprintln!("cannot create {}: {e}", dir.display());
        return ExitCode::FAILURE;
    }
    for (name, chain) in gex_fixtures::presets::golden() {
        let path = dir.join(format!("{name}.chain.json"));
        let json = match serde_json::to_string_pretty(&chain) {
            Ok(j) => j + "\n",
            Err(e) => {
                eprintln!("cannot serialize {name}: {e}");
                return ExitCode::FAILURE;
            }
        };
        if let Err(e) = std::fs::write(&path, json) {
            eprintln!("cannot write {}: {e}", path.display());
            return ExitCode::FAILURE;
        }
        println!("{} ({} contracts)", path.display(), chain.contracts().len());
    }
    ExitCode::SUCCESS
}
