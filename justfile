set windows-shell := ["pwsh.exe", "-NoLogo", "-NonInteractive", "-Command"]

# Default — list recipes.
default:
	just --list

# Build release binary into ./bin
build:
	cargo build --release
	@if (Test-Path target/release/wiki.exe) { New-Item -ItemType Directory -Force -Path bin | Out-Null; Copy-Item target/release/wiki.exe bin/wiki.exe -Force } elseif (Test-Path target/release/wiki) { New-Item -ItemType Directory -Force -Path bin | Out-Null; Copy-Item target/release/wiki bin/wiki -Force }

# Kill any running wiki.exe processes (Windows + Unix safe).
kill:
	@try { Get-Process wiki -ErrorAction Stop | Stop-Process -Force; Write-Host "Killed running wiki.exe" } catch { Write-Host "No wiki.exe running" }

# Install the freshly-built binary into ~/.cargo/bin (overwrites).
install: build
	cargo install --path . --force

# Update Claude plugin to latest from marketplace (best-effort — non-fatal if claude CLI missing).
update-plugin:
	@try { claude plugin update wiki@yesitsfebreeze; Write-Host "Plugin updated" } catch { Write-Host "claude CLI not found or update failed — skipping plugin update" }

# One-shot: kill running instances → rebuild → reinstall → refresh plugin.
update: kill install update-plugin
	@Write-Host "✅ wiki updated. Restart any MCP clients to pick up the new binary."

# Run the full test suite (single-threaded — required for shared-cache tests).
test:
	cargo test -- --test-threads=1

# Quick build + run for local debugging.
run:
	cargo run --release
