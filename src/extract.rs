use anyhow::Result;
use std::path::Path;

pub fn extract_pdf(pdf_path: &Path) -> Result<(String, Vec<String>)> {
    let path_str = pdf_path.to_string_lossy();
    Ok((
        format!("PDF extraction for {} - delegated to skill", path_str),
        vec![],
    ))
}

pub fn extract_youtube(url: &str) -> Result<String> {
    Ok(format!("YouTube extraction for {} - delegated to skill", url))
}
