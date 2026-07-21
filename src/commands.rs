use anyhow::{Context, Result, bail};
use serde::Serialize;

use crate::{
    backend::{self, ConvertRequest},
    cli::{BackendChoice, Command},
    dwg,
};

#[derive(Debug, Serialize)]
struct InspectOutput {
    #[serde(flatten)]
    file: dwg::DwgInfo,

    #[cfg(feature = "native-backend")]
    #[serde(skip_serializing_if = "Option::is_none")]
    native: Option<backend::native::NativeInspection>,

    #[cfg(feature = "native-backend")]
    #[serde(skip_serializing_if = "Option::is_none")]
    native_error: Option<String>,
}

pub fn execute(command: Command) -> Result<()> {
    match command {
        Command::Doctor { json } => backend::doctor(json),
        Command::Inspect { input, json } => {
            let info = dwg::inspect(&input)
                .with_context(|| format!("failed to inspect {}", input.display()))?;

            #[cfg(feature = "native-backend")]
            let (native, native_error) = match backend::native::inspect(&input) {
                Ok(native) => (Some(native), None),
                Err(error) => (None, Some(format!("{error:#}"))),
            };

            let output = InspectOutput {
                file: info,
                #[cfg(feature = "native-backend")]
                native,
                #[cfg(feature = "native-backend")]
                native_error,
            };

            if json {
                println!("{}", serde_json::to_string_pretty(&output)?);
            } else {
                println!("Path: {}", output.file.path);
                println!("Signature: {}", output.file.signature);
                println!("AutoCAD generation: {}", output.file.autocad_generation);
                println!("Size: {} bytes", output.file.size_bytes);
                println!("SHA-256: {}", output.file.sha256);

                #[cfg(feature = "native-backend")]
                {
                    if let Some(native) = &output.native {
                        for line in native.human_lines() {
                            println!("{line}");
                        }
                    }
                    if let Some(error) = &output.native_error {
                        println!("Native inspection failed: {error}");
                    }
                }
            }

            Ok(())
        }
        #[cfg(feature = "native-backend")]
        Command::Layers { input, json } => {
            let report = backend::native::layers(&input)
                .with_context(|| format!("failed to read layers from {}", input.display()))?;

            if json {
                println!("{}", serde_json::to_string_pretty(&report)?);
            } else {
                for line in report.human_lines() {
                    println!("{line}");
                }
            }

            Ok(())
        }
        #[cfg(not(feature = "native-backend"))]
        Command::Layers { .. } => {
            bail!(
                "the `layers` command uses the native backend; rebuild with --features native-backend"
            )
        }
        Command::Convert {
            input,
            output,
            backend,
            source_crs,
            target_crs,
            allow_local_coordinates,
            force,
            keep_intermediate,
            include_layers,
            exclude_layers,
        } => {
            if source_crs.is_none() && !allow_local_coordinates {
                bail!(
                    "a source CRS is required; pass --source-crs <CRS>, or explicitly accept raw drawing coordinates with --allow-local-coordinates"
                );
            }

            if allow_local_coordinates && (!include_layers.is_empty() || !exclude_layers.is_empty())
            {
                bail!(
                    "layer filtering runs on the GDAL route and requires --source-crs; it cannot be combined with --allow-local-coordinates"
                );
            }

            if allow_local_coordinates {
                eprintln!(
                    "warning: exporting local CAD coordinates without establishing a geographic CRS"
                );
            }

            let request = ConvertRequest {
                input: &input,
                output: &output,
                source_crs: source_crs.as_deref(),
                target_crs: &target_crs,
                allow_local_coordinates,
                force,
                keep_intermediate,
                include_layers: &include_layers,
                exclude_layers: &exclude_layers,
            };

            match backend {
                BackendChoice::External => backend::convert_external(&request),
                BackendChoice::Native => backend::convert_native(&request),
            }
        }
    }
}
