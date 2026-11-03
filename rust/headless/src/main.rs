use std::io;

fn main() {
    let stdin = io::stdin();
    let stdout = io::stdout();
    if let Err(error) = chasm_revm_headless::serve(stdin.lock(), stdout.lock()) {
        eprintln!("chasm REVM headless server failed: {error}");
        std::process::exit(1);
    }
}
