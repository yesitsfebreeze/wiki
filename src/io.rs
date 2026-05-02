use anyhow::{anyhow, Result};
use std::fs::File;
use std::io::Write;
use std::path::Path;

/// Write `bytes` to `path` durably: write to a sibling `.tmp` file, fsync,
/// then atomic rename. Survives mid-write crashes.
pub fn write_atomic(path: &Path, bytes: &[u8]) -> Result<()> {
	let parent = path
		.parent()
		.ok_or_else(|| anyhow!("path has no parent: {}", path.display()))?;
	if !parent.as_os_str().is_empty() {
		std::fs::create_dir_all(parent)?;
	}
	let file_name = path
		.file_name()
		.ok_or_else(|| anyhow!("path has no file name: {}", path.display()))?
		.to_string_lossy();
	let tmp = parent.join(format!(".{}.tmp", file_name));
	{
		let mut f = File::create(&tmp)?;
		f.write_all(bytes)?;
		f.sync_all()?;
	}
	// rename is atomic on POSIX; on Windows we fall back to remove+rename.
	#[cfg(windows)]
	{
		if path.exists() {
			let _ = std::fs::remove_file(path);
		}
	}
	std::fs::rename(&tmp, path)?;
	Ok(())
}

pub fn write_atomic_str(path: &Path, s: &str) -> Result<()> {
	write_atomic(path, s.as_bytes())
}

/// Serialize an `[f32]` slice as little-endian bytes via atomic write.
pub fn write_vec_f32(path: &Path, v: &[f32]) -> Result<()> {
	let mut bytes = Vec::with_capacity(v.len() * 4);
	for f in v {
		bytes.extend_from_slice(&f.to_le_bytes());
	}
	write_atomic(path, &bytes)
}

/// Read a previously-written `[f32]` slice. Optional `expected_dim` enforces
/// dimensionality on read.
pub fn read_vec_f32(path: &Path, expected_dim: Option<usize>) -> Result<Vec<f32>> {
	let bytes = std::fs::read(path)?;
	if bytes.len() % 4 != 0 {
		return Err(anyhow!("corrupt vec file: {}", path.display()));
	}
	let mut out = Vec::with_capacity(bytes.len() / 4);
	for chunk in bytes.chunks_exact(4) {
		out.push(f32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]));
	}
	if let Some(d) = expected_dim {
		if out.len() != d {
			return Err(anyhow!(
				"unexpected vec dim {} (expected {}) in {}",
				out.len(),
				d,
				path.display()
			));
		}
	}
	Ok(out)
}

/// FNV-1a 64-bit hash. Stable across runs, used for content fingerprints.
pub fn fnv64(s: &str) -> u64 {
	let mut h: u64 = 0xcbf29ce484222325;
	for b in s.bytes() {
		h ^= b as u64;
		h = h.wrapping_mul(0x100000001b3);
	}
	h
}
