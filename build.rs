use std::{env, fs, io, path::PathBuf};

use clap_complete::{Shell, generate_to};

// `src/cli.rs` depends only on this value enum from the runtime backend. Keeping
// the build-script copy small lets packaging assets use the authoritative derive
// tree without compiling either conversion backend.
#[allow(dead_code)]
mod backend {
    use clap::ValueEnum;

    #[derive(Clone, Copy, Debug, Eq, PartialEq, ValueEnum)]
    pub enum OutputFormat {
        #[value(name = "geojson")]
        GeoJson,

        #[value(name = "geojson-seq")]
        GeoJsonSeq,
    }
}

#[allow(dead_code)]
#[path = "src/cli.rs"]
mod cli;

fn main() -> io::Result<()> {
    println!("cargo:rerun-if-changed=Cargo.toml");
    println!("cargo:rerun-if-changed=src/cli.rs");

    let out_dir = env::var_os("OUT_DIR").ok_or(io::ErrorKind::NotFound)?;
    let asset_dir = PathBuf::from(out_dir).join("assets");
    fs::create_dir_all(&asset_dir)?;

    for shell in [Shell::Bash, Shell::Zsh, Shell::Fish, Shell::PowerShell] {
        generate_to(shell, &mut cli::command(), "dwg2geo", &asset_dir)?;
    }

    // clap_mangen 0.3 generates the top-level page and one page per subcommand.
    clap_mangen::generate_to(cli::command(), &asset_dir)?;

    println!(
        "cargo:warning=dwg2geo shell completions and man pages generated in {}",
        asset_dir.display()
    );
    println!("cargo:rustc-env=DWG2GEO_ASSET_DIR={}", asset_dir.display());

    Ok(())
}
