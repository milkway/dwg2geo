use std::{
    ffi::OsStr,
    fs,
    path::{Path, PathBuf},
    process::Command,
    time::Instant,
};

use anyhow::{Context, Result, anyhow, bail};
use serde::Serialize;
use tempfile::{TempDir, tempdir};

use super::{
    ConvertRequest,
    tools::{self, ToolInfo},
};
use crate::{
    dwg,
    report::{self, ConversionOptions, ConversionReport, Generator, OutputInfo, Step},
};

#[derive(Debug, Serialize)]
struct DoctorReport {
    healthy: bool,
    tools: Vec<ToolInfo>,
}

pub fn doctor(json: bool) -> Result<()> {
    let tools = tools::probe_external_tools();
    let healthy = tools
        .iter()
        .all(|tool| !tool.required || tool.is_available());

    if json {
        let report = DoctorReport { healthy, tools };
        println!("{}", serde_json::to_string_pretty(&report)?);
        if !report.healthy {
            bail!("required external tools are not usable; see the JSON tool list");
        }
        return Ok(());
    }

    for tool in &tools {
        println!("{}: {}", tool.name, tool.human_status());
    }

    if !healthy {
        let missing: Vec<&str> = tools
            .iter()
            .filter(|tool| tool.required && !tool.is_available())
            .map(|tool| tool.name.as_str())
            .collect();
        bail!(
            "the external backend requires {} to be installed and working",
            missing.join(", ")
        );
    }

    Ok(())
}

pub fn convert(request: &ConvertRequest<'_>) -> Result<()> {
    let started = Instant::now();

    validate_input(request.input)?;
    check_output_collision(request.output, request.force)?;
    ensure_parent_directory(request.output)?;

    let source = dwg::inspect(request.input)
        .with_context(|| format!("cannot inspect input {}", request.input.display()))?;

    let mut warnings = Vec::new();
    if source.autocad_generation.contains("unknown") {
        warnings.push(format!(
            "input signature {:?} is not a known DWG generation; the external tools may reject it",
            source.signature
        ));
    }
    if request.allow_local_coordinates {
        warnings.push(
            "output uses raw drawing coordinates; no geographic CRS was established".to_string(),
        );
    }

    let external_tools = tools::probe_external_tools();

    // The finished file is renamed into place only after the pipeline and the
    // non-empty check succeed, so a failed run never leaves a partial output
    // and --force never destroys the previous output before a replacement
    // exists.
    let partial = append_suffix(request.output, ".partial");
    remove_stale(&partial)?;

    let mut steps = Vec::new();
    let run = if let Some(source_crs) = request.source_crs {
        convert_with_reprojection(request, source_crs, &partial, &mut steps, &mut warnings)
    } else if request.allow_local_coordinates {
        convert_local_coordinates(request, &partial, &mut steps, &mut warnings)
    } else {
        bail!(
            "internal validation error: neither a source CRS nor local-coordinate permission was provided"
        )
    };

    let run = run.and_then(|()| ensure_nonempty_output(&partial));
    if let Err(error) = run {
        let _ = fs::remove_file(&partial);
        return Err(error);
    }

    fs::rename(&partial, request.output).with_context(|| {
        format!(
            "cannot move finished output into place at {}",
            request.output.display()
        )
    })?;

    let output_size = fs::metadata(request.output).map(|m| m.len()).unwrap_or(0);
    let conversion_report = ConversionReport {
        report_version: report::REPORT_VERSION,
        generator: Generator::current(),
        source,
        options: ConversionOptions {
            backend: "external",
            source_crs: request.source_crs.map(str::to_string),
            target_crs: request
                .source_crs
                .is_some()
                .then(|| request.target_crs.to_string()),
            allow_local_coordinates: request.allow_local_coordinates,
            force: request.force,
            keep_intermediate: request.keep_intermediate,
            include_layers: request.include_layers.to_vec(),
            exclude_layers: request.exclude_layers.to_vec(),
        },
        external_tools,
        steps,
        warnings,
        output: OutputInfo {
            path: request.output.display().to_string(),
            size_bytes: output_size,
        },
        total_duration_ms: started.elapsed().as_millis() as u64,
    };

    let report_file = report::report_path(request.output);
    report::write(&conversion_report, &report_file)?;

    eprintln!("wrote {}", request.output.display());
    eprintln!("wrote report {}", report_file.display());

    Ok(())
}

fn convert_with_reprojection(
    request: &ConvertRequest<'_>,
    source_crs: &str,
    partial: &Path,
    steps: &mut Vec<Step>,
    _warnings: &mut Vec<String>,
) -> Result<()> {
    let (dxf_path, _temporary): (PathBuf, Option<TempDir>) = if request.keep_intermediate {
        (append_suffix(request.output, ".intermediate.dxf"), None)
    } else {
        let temporary = tempdir().context("cannot create temporary conversion directory")?;
        (temporary.path().join("intermediate.dxf"), Some(temporary))
    };

    let mut dwgread = Command::new("dwgread");
    dwgread
        .arg("-O")
        .arg("DXF")
        .arg("-o")
        .arg(&dxf_path)
        .arg(request.input);
    steps.push(run_checked(dwgread, "LibreDWG conversion to DXF")?);

    let mut ogr2ogr = Command::new("ogr2ogr");
    ogr2ogr
        .arg("-f")
        .arg("GeoJSON")
        .arg("-dim")
        .arg("XY")
        .arg("-s_srs")
        .arg(source_crs)
        .arg("-t_srs")
        .arg(request.target_crs);

    if let Some(clause) = layer_where_clause(request.include_layers, request.exclude_layers) {
        ogr2ogr.arg("-where").arg(clause);
    }

    ogr2ogr.arg(partial).arg(&dxf_path);
    steps.push(run_checked(ogr2ogr, "GDAL conversion and reprojection")?);

    if request.keep_intermediate {
        eprintln!("kept intermediate DXF {}", dxf_path.display());
    }

    Ok(())
}

fn convert_local_coordinates(
    request: &ConvertRequest<'_>,
    partial: &Path,
    steps: &mut Vec<Step>,
    warnings: &mut Vec<String>,
) -> Result<()> {
    if request.keep_intermediate {
        warnings.push(
            "the local-coordinates route produces GeoJSON directly; there is no intermediate DXF to keep"
                .to_string(),
        );
    }

    let mut dwgread = Command::new("dwgread");
    dwgread
        .arg("-O")
        .arg("GeoJSON")
        .arg("-o")
        .arg(partial)
        .arg(request.input);

    steps.push(run_checked(dwgread, "LibreDWG direct GeoJSON conversion")?);
    Ok(())
}

/// Restrict the GDAL route to a layer subset via an attribute filter on the
/// DXF driver's `Layer` field.
fn layer_where_clause(include: &[String], exclude: &[String]) -> Option<String> {
    if !include.is_empty() {
        Some(format!("Layer IN ({})", quoted_list(include)))
    } else if !exclude.is_empty() {
        Some(format!("Layer NOT IN ({})", quoted_list(exclude)))
    } else {
        None
    }
}

fn quoted_list(names: &[String]) -> String {
    names
        .iter()
        .map(|name| format!("'{}'", name.replace('\'', "''")))
        .collect::<Vec<_>>()
        .join(", ")
}

fn validate_input(input: &Path) -> Result<()> {
    if !input.is_file() {
        bail!("input is not a readable file: {}", input.display());
    }
    Ok(())
}

fn check_output_collision(output: &Path, force: bool) -> Result<()> {
    if output.exists() && !force {
        bail!(
            "output already exists: {}; pass --force to replace it",
            output.display()
        );
    }
    Ok(())
}

fn ensure_parent_directory(output: &Path) -> Result<()> {
    if let Some(parent) = output.parent() {
        if !parent.as_os_str().is_empty() {
            fs::create_dir_all(parent)
                .with_context(|| format!("cannot create output directory {}", parent.display()))?;
        }
    }
    Ok(())
}

fn append_suffix(path: &Path, suffix: &str) -> PathBuf {
    let mut name = path.as_os_str().to_owned();
    name.push(suffix);
    PathBuf::from(name)
}

fn remove_stale(partial: &Path) -> Result<()> {
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

fn ensure_nonempty_output(output: &Path) -> Result<()> {
    let metadata = fs::metadata(output)
        .with_context(|| format!("conversion did not create {}", output.display()))?;
    if metadata.len() == 0 {
        bail!("conversion created an empty output: {}", output.display());
    }
    Ok(())
}

fn run_checked(mut command: Command, purpose: &str) -> Result<Step> {
    let rendered = render_command(&command);
    let program = command.get_program().to_string_lossy().into_owned();

    let started = Instant::now();
    let output = command.output().map_err(|error| {
        if error.kind() == std::io::ErrorKind::NotFound {
            anyhow!(
                "{purpose} failed: `{program}` was not found in PATH; {}; run `dwg2geo doctor` to check the external tools",
                tools::install_hint(&program)
            )
        } else {
            anyhow!("failed to start {purpose} ({rendered}): {error}")
        }
    })?;
    let duration_ms = started.elapsed().as_millis() as u64;

    if output.status.success() {
        return Ok(Step {
            purpose: purpose.to_string(),
            command: rendered,
            duration_ms,
        });
    }

    let stderr = bounded_text(&output.stderr, 8_000);
    let stdout = bounded_text(&output.stdout, 2_000);

    bail!(
        "{purpose} failed\ncommand: {rendered}\nstatus: {}\nstderr: {}\nstdout: {}",
        output.status,
        stderr,
        stdout
    )
}

fn render_command(command: &Command) -> String {
    std::iter::once(command.get_program())
        .chain(command.get_args())
        .map(render_arg)
        .collect::<Vec<_>>()
        .join(" ")
}

fn render_arg(value: &OsStr) -> String {
    let text = value.to_string_lossy();
    if text.contains(char::is_whitespace) {
        format!("{:?}", text)
    } else {
        text.into_owned()
    }
}

fn bounded_text(bytes: &[u8], max_bytes: usize) -> String {
    let text = String::from_utf8_lossy(bytes);
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return "(empty)".to_string();
    }
    if trimmed.len() <= max_bytes {
        return trimmed.to_string();
    }

    let mut boundary = max_bytes;
    while !trimmed.is_char_boundary(boundary) {
        boundary -= 1;
    }
    format!("{}… [truncated]", trimmed[..boundary].trim())
}

#[cfg(test)]
mod tests {
    use std::{ffi::OsStr, path::Path};

    use super::{append_suffix, bounded_text, layer_where_clause, render_arg};

    #[test]
    fn quotes_arguments_with_spaces() {
        assert_eq!(
            render_arg(OsStr::new("Corredor Sul.dwg")),
            "\"Corredor Sul.dwg\""
        );
    }

    #[test]
    fn truncates_long_subprocess_output() {
        let value = bounded_text(b"abcdefghij", 5);
        assert!(value.starts_with("abcde"));
        assert!(value.contains("truncated"));
    }

    #[test]
    fn reports_empty_subprocess_output_explicitly() {
        assert_eq!(bounded_text(b"  \n ", 100), "(empty)");
    }

    #[test]
    fn builds_include_clause_and_escapes_quotes() {
        let clause = layer_where_clause(&["EIXO".to_string(), "d'água".to_string()], &[]);
        assert_eq!(clause.as_deref(), Some("Layer IN ('EIXO', 'd''água')"));
    }

    #[test]
    fn builds_exclude_clause() {
        let clause = layer_where_clause(&[], &["MOLDURA".to_string()]);
        assert_eq!(clause.as_deref(), Some("Layer NOT IN ('MOLDURA')"));
    }

    #[test]
    fn no_layer_filter_means_no_clause() {
        assert_eq!(layer_where_clause(&[], &[]), None);
    }

    #[test]
    fn append_suffix_keeps_full_file_name() {
        assert_eq!(
            append_suffix(Path::new("out/plan a.geojson"), ".partial"),
            Path::new("out/plan a.geojson.partial")
        );
    }
}
