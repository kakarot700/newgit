//! newgit CLI entry point. All logic lives in the library (`newgit::cli`) so
//! integration tests can drive either the binary or the API.

fn main() {
    let argv: Vec<String> = std::env::args().collect();
    let code = newgit::cli::run(argv);
    std::process::exit(code);
}
