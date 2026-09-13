use std::io;

fn main() {
    let stdin = io::stdin();
    let stdout = io::stdout();
    if let Err(error) = sevm_revm_headless::serve(stdin.lock(), stdout.lock()) {
        eprintln!("sevm REVM headless server failed: {error}");
        std::process::exit(1);
    }
}
