use std::path::PathBuf;

use clap::{Parser, Subcommand, ValueEnum};

#[derive(Debug, Parser)]
#[command(name = "dwg2geo", version, about)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Command,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    /// Check optional external conversion tools.
    Doctor {
        /// Emit JSON instead of human-readable text.
        #[arg(long)]
        json: bool,
    },

    /// Inspect stable DWG file-level metadata without parsing drawing entities.
    Inspect {
        /// Input DWG file.
        input: PathBuf,

        /// Emit JSON instead of human-readable text.
        #[arg(long)]
        json: bool,
    },

    /// Convert a DWG drawing to GeoJSON.
    Convert {
        /// Input DWG file.
        input: PathBuf,

        /// Output GeoJSON path.
        #[arg(short, long)]
        output: PathBuf,

        /// Conversion backend.
        #[arg(long, value_enum, default_value = "external")]
        backend: BackendChoice,

        /// Source CRS accepted by GDAL/PROJ, for example EPSG:31985.
        #[arg(long)]
        source_crs: Option<String>,

        /// Target CRS. RFC 7946 GeoJSON should normally remain EPSG:4326.
        #[arg(long, default_value = "EPSG:4326")]
        target_crs: String,

        /// Explicitly allow non-geographic/local CAD coordinates.
        #[arg(long, conflicts_with = "source_crs")]
        allow_local_coordinates: bool,

        /// Replace an existing output file.
        #[arg(long)]
        force: bool,

        /// Keep the intermediate DXF next to the output for diagnostics.
        #[arg(long)]
        keep_intermediate: bool,

        /// Convert only these layers (comma-separated). GDAL route only.
        #[arg(long, value_delimiter = ',', value_name = "LAYER")]
        include_layers: Vec<String>,

        /// Skip these layers (comma-separated). GDAL route only.
        #[arg(
            long,
            value_delimiter = ',',
            value_name = "LAYER",
            conflicts_with = "include_layers"
        )]
        exclude_layers: Vec<String>,
    },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, ValueEnum)]
pub enum BackendChoice {
    /// Use the installed LibreDWG and GDAL command-line tools.
    External,

    /// Use the optional native Rust backend. This is a roadmap feature.
    Native,
}
