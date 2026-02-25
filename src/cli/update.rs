use anyhow::{Context, Result, bail};
use clap::Args;
use colored::Colorize;
use serde::Deserialize;
use std::io::Read;

/// GitHub repository used to fetch releases for self-update.
const GITHUB_REPO: &str = "liberuum/switchboard-cli";
/// Current version compiled from Cargo.toml at build time.
const CURRENT_VERSION: &str = env!("CARGO_PKG_VERSION");

#[derive(Args)]
pub struct UpdateArgs {
    /// Only check for updates, don't install
    #[arg(long)]
    pub check: bool,
}

// ── GitHub API types ────────────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
struct Release {
    tag_name: String,
    body: Option<String>,
    assets: Vec<Asset>,
}

#[derive(Debug, Deserialize)]
struct Asset {
    name: String,
    browser_download_url: String,
}

// ── Version helpers ─────────────────────────────────────────────────────────

fn parse_version(tag: &str) -> Option<(u32, u32, u32)> {
    let v = tag.strip_prefix('v').unwrap_or(tag);
    let parts: Vec<&str> = v.split('.').collect();
    if parts.len() == 3 {
        Some((
            parts[0].parse().ok()?,
            parts[1].parse().ok()?,
            parts[2].parse().ok()?,
        ))
    } else {
        None
    }
}

fn is_newer(latest: &str, current: &str) -> bool {
    match (parse_version(latest), parse_version(current)) {
        (Some(l), Some(c)) => l > c,
        _ => false,
    }
}

fn platform_suffix() -> Result<&'static str> {
    let os = std::env::consts::OS;
    let arch = std::env::consts::ARCH;
    match (os, arch) {
        ("macos", "aarch64") => Ok("darwin-aarch64.tar.gz"),
        ("linux", "x86_64") => Ok("linux-x86_64.tar.gz"),
        _ => bail!(
            "Unsupported platform: {os}-{arch}. \
             Pre-built binaries are available for macOS ARM64 and Linux x86_64."
        ),
    }
}

/// Strip the "## Install" boilerplate from release notes, keeping only real changelog content.
fn strip_install_section(body: &str) -> String {
    let mut out = String::new();
    let mut skip = false;
    for line in body.lines() {
        if line.starts_with("## Install") {
            skip = true;
            continue;
        }
        if skip && line.starts_with("## ") {
            skip = false;
        }
        if !skip {
            out.push_str(line);
            out.push('\n');
        }
    }
    out.trim().to_string()
}

// ── Entry point ─────────────────────────────────────────────────────────────

/// Check for updates and optionally self-update the binary.
///
/// Queries the GitHub Releases API, compares versions, displays a changelog
/// covering all intermediate releases, and (unless `check` is true) downloads
/// and atomically replaces the running binary. If the binary lives in a
/// system directory, falls back to `sudo cp` and prompts the user for their
/// password.
pub async fn run(check: bool, quiet: bool) -> Result<()> {
    let client = reqwest::Client::builder()
        .user_agent("switchboard-cli")
        .build()?;

    if !quiet {
        eprintln!("Checking for updates...");
    }

    let releases: Vec<Release> = client
        .get(format!(
            "https://api.github.com/repos/{GITHUB_REPO}/releases"
        ))
        .send()
        .await
        .context("Failed to check for updates")?
        .json()
        .await
        .context("Failed to parse release info")?;

    if releases.is_empty() {
        bail!("No releases found on GitHub");
    }

    let latest = &releases[0];
    let latest_tag = &latest.tag_name;
    let current_tag = format!("v{CURRENT_VERSION}");

    if !is_newer(latest_tag, &current_tag) {
        println!(
            "{} Already on latest version ({})",
            "✓".green(),
            current_tag
        );
        return Ok(());
    }

    // ── Show version diff ───────────────────────────────────────────────

    println!("Current version: {}", current_tag.yellow());
    println!("Latest version:  {}", latest_tag.green());
    println!();

    // ── Show changelog for all intermediate versions ────────────────────

    let newer_releases: Vec<&Release> = releases
        .iter()
        .filter(|r| is_newer(&r.tag_name, &current_tag))
        .collect();

    if !newer_releases.is_empty() {
        println!("{}", "Changelog:".bold());
        println!();
        // Show oldest first so the reader sees changes in chronological order
        for release in newer_releases.iter().rev() {
            println!("  {} {}", "─".dimmed(), release.tag_name.bold());
            if let Some(body) = &release.body {
                let cleaned = strip_install_section(body);
                if !cleaned.is_empty() {
                    for line in cleaned.lines() {
                        println!("    {line}");
                    }
                    println!();
                }
            }
        }
    }

    if check {
        return Ok(());
    }

    // ── Confirm ─────────────────────────────────────────────────────────

    let confirm = dialoguer::Confirm::new()
        .with_prompt(format!("Update to {latest_tag}?"))
        .default(true)
        .interact()?;

    if !confirm {
        println!("Aborted.");
        return Ok(());
    }

    // ── Download ────────────────────────────────────────────────────────

    let suffix = platform_suffix()?;
    let asset = latest
        .assets
        .iter()
        .find(|a| a.name.ends_with(suffix))
        .ok_or_else(|| {
            anyhow::anyhow!("No binary found for this platform ({suffix}) in release {latest_tag}")
        })?;

    eprintln!("Downloading {}...", asset.name);

    let bytes = client
        .get(&asset.browser_download_url)
        .send()
        .await
        .context("Failed to download release")?
        .bytes()
        .await
        .context("Failed to read release archive")?;

    // ── Extract binary from tar.gz ──────────────────────────────────────

    let decoder = flate2::read::GzDecoder::new(&bytes[..]);
    let mut archive = tar::Archive::new(decoder);

    let temp_dir = std::env::temp_dir();
    let temp_path = temp_dir.join("switchboard-update");

    let mut found = false;
    for entry in archive.entries().context("Failed to read tar archive")? {
        let mut entry = entry?;
        let path = entry.path()?;
        if path
            .file_name()
            .map(|n| n == "switchboard")
            .unwrap_or(false)
        {
            let mut buf = Vec::new();
            entry.read_to_end(&mut buf)?;
            std::fs::write(&temp_path, &buf)?;
            found = true;
            break;
        }
    }

    if !found {
        let _ = std::fs::remove_file(&temp_path);
        bail!("Could not find 'switchboard' binary inside the release archive");
    }

    // Make executable
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&temp_path, std::fs::Permissions::from_mode(0o755))?;
    }

    // ── Replace binary ────────────────────────────────────────────────

    let exe_path = std::env::current_exe().context("Could not determine current binary path")?;

    // Try direct replacement first; fall back to sudo if permission denied
    if self_replace::self_replace(&temp_path).is_err() {
        eprintln!("Need elevated permissions to update {}", exe_path.display());
        let status = std::process::Command::new("sudo")
            .args(["cp", "-f"])
            .arg(&temp_path)
            .arg(&exe_path)
            .status()
            .context("Failed to run sudo")?;
        if !status.success() {
            let _ = std::fs::remove_file(&temp_path);
            bail!("Failed to replace binary with sudo");
        }
    }

    let _ = std::fs::remove_file(&temp_path);

    // Clear macOS quarantine attribute
    #[cfg(target_os = "macos")]
    {
        let _ = std::process::Command::new("sudo")
            .args(["xattr", "-d", "com.apple.quarantine"])
            .arg(&exe_path)
            .output();
    }

    println!(
        "{} Updated switchboard {} → {}",
        "✓".green(),
        current_tag,
        latest_tag.green()
    );

    Ok(())
}
