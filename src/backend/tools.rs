use std::process::{Command, Output};

use serde::Serialize;

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolStatus {
    /// The tool responded to `--version` successfully.
    Available,
    /// The tool was not found in PATH.
    Missing,
    /// The tool was found but exited with a failure status.
    Unhealthy,
    /// The tool could not be executed for another reason.
    Unavailable,
}

#[derive(Clone, Debug, Serialize)]
pub struct ToolInfo {
    pub name: String,
    pub role: String,
    pub required: bool,
    pub status: ToolStatus,
    pub version: Option<String>,
    pub detail: Option<String>,
}

impl ToolInfo {
    pub fn is_available(&self) -> bool {
        self.status == ToolStatus::Available
    }

    pub fn human_status(&self) -> String {
        match (self.status, &self.version, &self.detail) {
            (ToolStatus::Available, Some(version), _) => format!("available ({version})"),
            (ToolStatus::Available, None, _) => "available".to_string(),
            (ToolStatus::Missing, _, Some(detail)) => format!("missing; {detail}"),
            (ToolStatus::Missing, _, None) => "missing".to_string(),
            (ToolStatus::Unhealthy, _, Some(detail)) => format!("unhealthy ({detail})"),
            (ToolStatus::Unhealthy, _, None) => "unhealthy".to_string(),
            (ToolStatus::Unavailable, _, Some(detail)) => format!("unavailable ({detail})"),
            (ToolStatus::Unavailable, _, None) => "unavailable".to_string(),
        }
    }
}

/// Probe the external tools in a fixed, deterministic order.
pub fn probe_external_tools() -> Vec<ToolInfo> {
    vec![
        probe(
            "dwgread",
            "GNU LibreDWG reader; converts DWG to DXF or GeoJSON",
            true,
        ),
        probe(
            "ogr2ogr",
            "GDAL vector converter; required when --source-crs reprojection is requested",
            false,
        ),
    ]
}

pub fn probe(name: &str, role: &str, required: bool) -> ToolInfo {
    let base = |status, version, detail| ToolInfo {
        name: name.to_string(),
        role: role.to_string(),
        required,
        status,
        version,
        detail,
    };

    match Command::new(name).arg("--version").output() {
        Ok(output) if output.status.success() => {
            base(ToolStatus::Available, first_nonempty_line(&output), None)
        }
        Ok(output) => base(
            ToolStatus::Unhealthy,
            None,
            Some(format!("`{name} --version` exited with {}", output.status)),
        ),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            base(ToolStatus::Missing, None, Some(install_hint(name)))
        }
        Err(error) => base(ToolStatus::Unavailable, None, Some(error.to_string())),
    }
}

pub fn install_hint(name: &str) -> String {
    match name {
        "dwgread" => "install GNU LibreDWG to provide the `dwgread` command".to_string(),
        "ogr2ogr" => "install GDAL to provide the `ogr2ogr` command".to_string(),
        other => format!("install the `{other}` command"),
    }
}

fn first_nonempty_line(output: &Output) -> Option<String> {
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);

    stdout
        .lines()
        .chain(stderr.lines())
        .find(|line| !line.trim().is_empty())
        .map(|line| line.trim().to_string())
}

#[cfg(test)]
mod tests {
    use super::{ToolStatus, probe};

    #[test]
    fn probing_a_missing_tool_reports_missing_with_hint() {
        let info = probe("definitely-not-a-real-tool-a6f3", "test tool", true);
        assert_eq!(info.status, ToolStatus::Missing);
        assert!(
            info.detail
                .expect("hint")
                .contains("definitely-not-a-real-tool-a6f3")
        );
        assert!(info.version.is_none());
    }

    #[test]
    fn probing_cargo_reports_available_with_version() {
        let info = probe(
            "cargo",
            "build tool used as an always-present fixture",
            false,
        );
        assert_eq!(info.status, ToolStatus::Available);
        assert!(info.version.expect("version line").contains("cargo"));
    }
}
