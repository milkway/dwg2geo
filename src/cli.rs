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

    /// List layers with entity counts by type and space (native backend).
    Layers {
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

        /// Emit closed polylines as Polygons instead of closed LineStrings
        /// (native backend only; see ADR-006).
        #[arg(long)]
        polygonize_closed: bool,

        /// Maximum chord error, in drawing units, when tessellating arcs
        /// (native backend only; default 0.05).
        #[arg(long, value_name = "UNITS")]
        curve_tolerance: Option<f64>,

        /// Expand INSERT references into their block geometry. This is the
        /// default; the flag exists to make the choice explicit.
        #[arg(long, conflicts_with = "preserve_inserts")]
        explode_blocks: bool,

        /// Emit INSERT references as point features with block name and
        /// attributes instead of expanding them (native backend only).
        #[arg(long)]
        preserve_inserts: bool,
    },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, ValueEnum)]
pub enum BackendChoice {
    /// Use the installed LibreDWG and GDAL command-line tools.
    External,

    /// Use the optional native Rust backend. This is a roadmap feature.
    Native,
}
