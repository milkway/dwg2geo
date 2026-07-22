mod external;
#[cfg(feature = "native-backend")]
pub mod native;
pub mod tools;

use std::{
    fs,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result, bail};
use clap::ValueEnum;

#[derive(Clone, Copy, Debug, Eq, PartialEq, ValueEnum)]
pub enum OutputFormat {
    #[value(name = "geojson")]
    GeoJson,

    #[value(name = "geojson-seq")]
    GeoJsonSeq,
}

impl std::fmt::Display for OutputFormat {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::GeoJson => "geojson",
            Self::GeoJsonSeq => "geojson-seq",
        })
    }
}

pub struct ConvertRequest<'a> {
    pub input: &'a Path,
    pub output: &'a Path,
    pub source_crs: Option<&'a str>,
    pub target_crs: &'a str,
    pub allow_local_coordinates: bool,
    pub force: bool,
    pub keep_intermediate: bool,
    pub include_layers: &'a [String],
    pub exclude_layers: &'a [String],
    pub output_format: OutputFormat,
    pub source_units: Option<&'a str>,
    pub allow_suspect_extents: bool,
    pub control_points: &'a [String],
    pub polygonize_closed: bool,
    pub curve_tolerance: Option<f64>,
    pub preserve_inserts: bool,
}

pub fn doctor(json: bool) -> Result<()> {
    external::doctor(json)
}

pub fn convert_external(request: &ConvertRequest<'_>) -> Result<()> {
    external::convert(request)
}

#[cfg(feature = "native-backend")]
pub fn convert_native(request: &ConvertRequest<'_>) -> Result<()> {
    native::convert::convert(request)
}

#[cfg(not(feature = "native-backend"))]
pub fn convert_native(_request: &ConvertRequest<'_>) -> Result<()> {
    bail!(
        "the native backend is not built; rebuild with --features native-backend to convert without external tools"
    )
}

// Output-lifecycle helpers shared by both backends: outputs are produced at
// `<output>.partial` and renamed into place only once complete, so failures
// never leave partial files and --force never destroys the previous output
// before a replacement exists.

pub(crate) fn validate_input(input: &Path) -> Result<()> {
    if !input.is_file() {
        bail!("input is not a readable file: {}", input.display());
    }
    Ok(())
}

pub(crate) fn check_output_collision(output: &Path, force: bool) -> Result<()> {
    if output.exists() && !force {
        bail!(
            "output already exists: {}; pass --force to replace it",
            output.display()
        );
    }
    Ok(())
}

pub(crate) fn ensure_parent_directory(output: &Path) -> Result<()> {
    if let Some(parent) = output.parent() {
        if !parent.as_os_str().is_empty() {
            fs::create_dir_all(parent)
                .with_context(|| format!("cannot create output directory {}", parent.display()))?;
        }
    }
    Ok(())
}

pub(crate) fn append_suffix(path: &Path, suffix: &str) -> PathBuf {
    let mut name = path.as_os_str().to_owned();
    name.push(suffix);
    PathBuf::from(name)
}

pub(crate) fn remove_stale(partial: &Path) -> Result<()> {
    if partial.exists() {
        fs::remove_file(partial).with_context(|| {
            format!(
                "cannot remove stale partial output {} from a previous run",
                partial.display()
            )
        })?;
    }
    Ok(())
}

pub(crate) fn ensure_nonempty_output(output: &Path) -> Result<()> {
    let metadata = fs::metadata(output)
        .with_context(|| format!("conversion did not create {}", output.display()))?;
    if metadata.len() == 0 {
        bail!("conversion created an empty output: {}", output.display());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::append_suffix;

    #[test]
    fn append_suffix_keeps_full_file_name() {
        assert_eq!(
            append_suffix(Path::new("out/plan a.geojson"), ".partial"),
            Path::new("out/plan a.geojson.partial")
        );
    }
}
