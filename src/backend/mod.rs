mod external;
#[cfg(feature = "native-backend")]
pub mod native;
pub mod tools;

use std::path::Path;

use anyhow::{Result, bail};

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
}

pub fn doctor(json: bool) -> Result<()> {
    external::doctor(json)
}

pub fn convert_external(request: &ConvertRequest<'_>) -> Result<()> {
    external::convert(request)
}

#[cfg(feature = "native-backend")]
pub fn convert_native(_request: &ConvertRequest<'_>) -> Result<()> {
    bail!(
        "the native backend feature is enabled, but entity conversion is not implemented yet; complete Milestones 2 and 3"
    )
}

#[cfg(not(feature = "native-backend"))]
pub fn convert_native(_request: &ConvertRequest<'_>) -> Result<()> {
    bail!(
        "the native backend is not built; rebuild with --features native-backend after implementing Milestones 2 and 3"
    )
}
