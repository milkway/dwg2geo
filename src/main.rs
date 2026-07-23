use anyhow::Result;
use clap::Parser;
use dwg2geo::cli::Cli;
use dwg2geo::commands;

fn main() {
    if let Err(error) = run() {
        eprintln!("error: {error:#}");
        std::process::exit(1);
    }
}

fn run() -> Result<()> {
    let cli = Cli::parse();
    commands::execute(cli.command)
}
