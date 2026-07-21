use anyhow::{Context, Result, bail};

use crate::{
    backend::{self, ConvertRequest},
    cli::{BackendChoice, Command},
    dwg,
};

pub fn execute(command: Command) -> Result<()> {
    match command {
        Command::Doctor => backend::doctor(),
        Command::Inspect { input, json } => {
            let info = dwg::inspect(&input)
                .with_context(|| format!("failed to inspect {}", input.display()))?;

            if json {
                println!("{}", serde_json::to_string_pretty(&info)?);
            } else {
                println!("Path: {}", info.path);
                println!("Signature: {}", info.signature);
                println!("AutoCAD generation: {}", info.autocad_generation);
                println!("Size: {} bytes", info.size_bytes);
                println!("SHA-256: {}", info.sha256);
            }

            Ok(())
        }
        Command::Convert {
            input,
            output,
            backend,
            source_crs,
            target_crs,
            allow_local_coordinates,
            force,
        } => {
            if source_crs.is_none() && !allow_local_coordinates {
                bail!(
                    "a source CRS is required; pass --source-crs <CRS>, or explicitly accept raw drawing coordinates with --allow-local-coordinates"
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
            };

            match backend {
                BackendChoice::External => backend::convert_external(&request),
                BackendChoice::Native => backend::convert_native(&request),
            }
        }
    }
}
