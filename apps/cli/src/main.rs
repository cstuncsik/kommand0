use clap::Parser;

#[derive(Parser)]
#[command(name = "kommand0", version, about = "Keyboard-first local orchestrator for parallel coding sessions")]
struct Cli {}

fn main() {
    let _cli = Cli::parse();
    println!("kommand0");
}
