use std::{
    ffi::OsStr,
    fs,
    path::Path,
    process::{Command, Output},
};

use anyhow::{Context, Result, bail};
use tempfile::tempdir;

use super::ConvertRequest;

pub fn doctor() -> Result<()> {
    let dwgread = tool_status("dwgread");
    let ogr2ogr = tool_status("ogr2ogr");

    println!("dwgread: {}", dwgread);
    println!("ogr2ogr: {}", ogr2ogr);

    if dwgread.starts_with("missing") {
        bail!("dwgread is required for the external backend");
    }

    Ok(())
}

pub fn convert(request: &ConvertRequest<'_>) -> Result<()> {
    validate_input(request.input)?;
    prepare_output(request.output, request.force)?;

    if let Some(source_crs) = request.source_crs {
        convert_with_reprojection(request, source_crs)
    } else if request.allow_local_coordinates {
        convert_local_coordinates(request)
    } else {
        bail!(
            "internal validation error: neither a source CRS nor local-coordinate permission was provided"
        )
    }
}

fn convert_with_reprojection(request: &ConvertRequest<'_>, source_crs: &str) -> Result<()> {
    let temporary = tempdir().context("cannot create temporary conversion directory")?;
    let dxf_path = temporary.path().join("intermediate.dxf");

    let mut dwgread = Command::new("dwgread");
    dwgread
        .arg("-O")
        .arg("DXF")
        .arg("-o")
        .arg(&dxf_path)
        .arg(request.input);
    run_checked(dwgread, "LibreDWG conversion to DXF")?;

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

    if request.force {
        ogr2ogr.arg("-overwrite");
    }

    ogr2ogr.arg(request.output).arg(&dxf_path);
    run_checked(ogr2ogr, "GDAL conversion and reprojection")?;

    ensure_nonempty_output(request.output)
}

fn convert_local_coordinates(request: &ConvertRequest<'_>) -> Result<()> {
    let mut dwgread = Command::new("dwgread");
    dwgread
        .arg("-O")
        .arg("GeoJSON")
        .arg("-o")
        .arg(request.output)
        .arg(request.input);

    run_checked(dwgread, "LibreDWG direct GeoJSON conversion")?;
    ensure_nonempty_output(request.output)
}

fn validate_input(input: &Path) -> Result<()> {
    if !input.is_file() {
        bail!("input is not a readable file: {}", input.display());
    }
    Ok(())
}

fn prepare_output(output: &Path, force: bool) -> Result<()> {
    if output.exists() {
        if !force {
            bail!(
                "output already exists: {}; pass --force to replace it",
                output.display()
            );
        }
        fs::remove_file(output)
            .with_context(|| format!("cannot remove existing output {}", output.display()))?;
    }

    if let Some(parent) = output.parent() {
        if !parent.as_os_str().is_empty() {
            fs::create_dir_all(parent)
                .with_context(|| format!("cannot create output directory {}", parent.display()))?;
        }
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

fn tool_status(program: &str) -> String {
    match Command::new(program).arg("--version").output() {
        Ok(output) if output.status.success() => {
            let text = first_nonempty_line(&output).unwrap_or_else(|| "available".to_string());
            format!("available ({text})")
        }
        Ok(output) => format!("unhealthy (exit {})", output.status),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => "missing".to_string(),
        Err(error) => format!("unavailable ({error})"),
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

fn run_checked(mut command: Command, purpose: &str) -> Result<()> {
    let rendered = render_command(&command);
    let output = command
        .output()
        .with_context(|| format!("failed to start {purpose}: {rendered}"))?;

    if output.status.success() {
        return Ok(());
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
    if text.len() <= max_bytes {
        return text.trim().to_string();
    }

    let mut boundary = max_bytes;
    while !text.is_char_boundary(boundary) {
        boundary -= 1;
    }
    format!("{}… [truncated]", text[..boundary].trim())
}

#[cfg(test)]
mod tests {
    use std::ffi::OsStr;

    use super::{bounded_text, render_arg};

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
}
