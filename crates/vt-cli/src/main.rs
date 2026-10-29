use clap::Parser;

/// AI kodlama aracı verilerini makineler arasında taşır.
#[derive(Parser)]
#[command(name = "vibe-transfer", version, arg_required_else_help = true)]
struct Cli {}

fn main() {
    let _cli = Cli::parse();
}
