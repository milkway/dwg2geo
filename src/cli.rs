use std::path::PathBuf;

use clap::{Parser, Subcommand, ValueEnum};

use crate::backend::OutputFormat;

#[derive(Debug, Parser)]
#[command(name = "dwg2geo", version, about)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Command,
}

// The Convert variant dwarfs the others; a single Command is parsed once
// per process, so boxing it would only add noise.
#[allow(clippy::large_enum_variant)]
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

        /// Output format (native backend only).
        #[arg(long, value_enum, default_value = "geojson")]
        output_format: OutputFormat,

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

        /// Linear unit of the drawing coordinates (m, mm, cm, dm, km, in,
        /// ft, usft). Required with --source-crs on the native backend when
        /// the drawing's own unit hints are absent, ambiguous, or
        /// inconsistent.
        #[arg(long, value_name = "UNIT", requires = "source_crs")]
        source_units: Option<String>,

        /// Deliver EPSG:4326 output even when coordinates fall outside the
        /// plausible longitude/latitude range (native backend only; the
        /// default is to fail closed on implausible extents).
        #[arg(long)]
        allow_suspect_extents: bool,

        /// Georeference by local similarity calibration instead of a source
        /// CRS: map drawing coordinates to target-CRS coordinates using at
        /// least two control points "DX,DY=X,Y" (native backend only;
        /// repeatable; three or more points additionally report residuals).
        #[arg(
            long = "control-point",
            value_name = "DX,DY=X,Y",
            conflicts_with_all = ["source_crs", "allow_local_coordinates"]
        )]
        control_points: Vec<String>,

        /// Check every output feature against a reference boundary polygon
        /// (GeoJSON Polygon/MultiPolygon in the OUTPUT coordinate system,
        /// e.g. an IBGE municipal boundary) and report containment counts
        /// (native backend only; informational, never fails the conversion).
        #[arg(long, value_name = "GEOJSON")]
        validate_boundary: Option<PathBuf>,
    },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, ValueEnum)]
pub enum BackendChoice {
    /// Use the installed LibreDWG and GDAL command-line tools.
    External,

    /// Use the optional native Rust backend. This is a roadmap feature.
    Native,
}
