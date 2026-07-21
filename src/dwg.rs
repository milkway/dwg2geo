use std::{
    fs::{self, File},
    io::{Read, Seek, SeekFrom},
    path::Path,
};

use anyhow::{Context, Result};
use serde::Serialize;
use sha2::{Digest, Sha256};

#[derive(Debug, Serialize)]
pub struct DwgInfo {
    pub path: String,
    pub signature: String,
    pub autocad_generation: String,
    pub size_bytes: u64,
    pub sha256: String,
}

pub fn inspect(path: &Path) -> Result<DwgInfo> {
    let metadata = fs::metadata(path)
        .with_context(|| format!("cannot read metadata for {}", path.display()))?;

    let mut file = File::open(path).with_context(|| format!("cannot open {}", path.display()))?;

    let mut header = [0_u8; 6];
    file.read_exact(&mut header)
        .context("file is too short to contain a DWG signature")?;

    let signature = String::from_utf8_lossy(&header).to_string();
    let autocad_generation = generation_for_signature(&signature)
        .unwrap_or("unknown or unsupported DWG signature")
        .to_string();

    file.seek(SeekFrom::Start(0))?;
    let mut hasher = Sha256::new();
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }

    Ok(DwgInfo {
        path: path.display().to_string(),
        signature,
        autocad_generation,
        size_bytes: metadata.len(),
        sha256: format!("{:x}", hasher.finalize()),
    })
}

fn generation_for_signature(signature: &str) -> Option<&'static str> {
    match signature {
        "AC1002" => Some("AutoCAD R2.5"),
        "AC1003" => Some("AutoCAD R2.6"),
        "AC1004" => Some("AutoCAD R9"),
        "AC1006" => Some("AutoCAD R10"),
        "AC1009" => Some("AutoCAD R11/R12"),
        "AC1012" => Some("AutoCAD R13"),
        "AC1014" => Some("AutoCAD R14"),
        "AC1015" => Some("AutoCAD 2000/2000i/2002"),
        "AC1018" => Some("AutoCAD 2004/2005/2006"),
        "AC1021" => Some("AutoCAD 2007/2008/2009"),
        "AC1024" => Some("AutoCAD 2010/2011/2012"),
        "AC1027" => Some("AutoCAD 2013/2014/2015/2016/2017"),
        "AC1032" => Some("AutoCAD 2018 and later generation"),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use std::io::Write;

    use tempfile::NamedTempFile;

    use super::inspect;

    #[test]
    fn identifies_ac1027() {
        let mut file = NamedTempFile::new().expect("temporary file");
        file.write_all(b"AC1027synthetic-test-content")
            .expect("write fixture");

        let info = inspect(file.path()).expect("inspect fixture");

        assert_eq!(info.signature, "AC1027");
        assert!(info.autocad_generation.contains("2013"));
        assert_eq!(info.size_bytes, 28);
        assert_eq!(info.sha256.len(), 64);
    }

    #[test]
    fn reports_unknown_signature() {
        let mut file = NamedTempFile::new().expect("temporary file");
        file.write_all(b"ZZ9999synthetic-test-content")
            .expect("write fixture");

        let info = inspect(file.path()).expect("inspect fixture");
        assert_eq!(info.signature, "ZZ9999");
        assert!(info.autocad_generation.contains("unknown"));
    }

    #[test]
    fn rejects_short_file() {
        let mut file = NamedTempFile::new().expect("temporary file");
        file.write_all(b"AC10").expect("write fixture");

        let error = inspect(file.path()).expect_err("short file must fail");
        assert!(error.to_string().contains("too short"));
    }
}
