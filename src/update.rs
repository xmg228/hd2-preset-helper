//! Stable release discovery and single-executable updates. No UI or game APIs.
use std::fs;
use std::path::Path;
use std::time::Duration;

use anyhow::{Context, Result, ensure};
use semver::Version;

const ASSET_API: &str = "https://api.github.com/repos/xmg228/hd2-preset-helper/releases/assets/";
const BINARY_IN_ARCHIVE: &str = "HD2PresetHelper/HD2PresetHelper.exe";

#[derive(Clone)]
pub struct AvailableUpdate {
    pub version: String,
    asset_url: String,
    sha256: String,
}

impl AvailableUpdate {
    pub fn notes_url(&self) -> String {
        format!(
            "{}/releases/tag/v{}",
            env!("CARGO_PKG_REPOSITORY"),
            self.version
        )
    }

    fn validate(&self, current: &str) -> Result<()> {
        ensure!(
            newer_stable(&self.version, current)?,
            "the selected update is not a newer stable version"
        );
        ensure!(
            self.asset_url
                .strip_prefix(ASSET_API)
                .and_then(|id| id.parse::<u64>().ok())
                .is_some_and(|id| id > 0),
            "the update asset does not belong to the official repository"
        );
        ensure!(
            self.sha256.len() == 64 && self.sha256.bytes().all(|c| c.is_ascii_hexdigit()),
            "the release asset has no valid SHA-256 digest"
        );
        Ok(())
    }

    fn from_release(release: &self_update::update::Release, current: &str) -> Result<Self> {
        let name = format!("HD2PresetHelper-v{}-windows-x64.zip", release.version());
        let asset = release
            .assets()
            .iter()
            .find(|asset| asset.name() == name)
            .context("the release has no Windows x64 update package")?;
        let update = Self {
            version: release.version().to_owned(),
            asset_url: asset.download_url().to_owned(),
            sha256: asset
                .digest()
                .and_then(|value| value.strip_prefix("sha256:"))
                .context("the release asset has no SHA-256 digest")?
                .to_owned(),
        };
        update.validate(current)?;
        Ok(update)
    }
}

pub const fn installation_supported() -> bool {
    cfg!(all(target_os = "windows", target_arch = "x86_64")) && !cfg!(feature = "diagnostics")
}

pub const fn automatic_checks_supported() -> bool {
    installation_supported() && !cfg!(debug_assertions)
}

pub fn check() -> Result<Option<AvailableUpdate>> {
    let updater = updater().timeout(Duration::from_secs(10)).build()?;
    let releases = updater.get_latest_release()?;
    let release = releases
        .latest()
        .context("no published stable release is available")?;
    if !newer_stable(release.version(), env!("CARGO_PKG_VERSION"))? {
        return Ok(None);
    }
    Ok(Some(AvailableUpdate::from_release(
        release,
        env!("CARGO_PKG_VERSION"),
    )?))
}

fn updater() -> self_update::backends::github::UpdateBuilder {
    let mut builder = self_update::backends::github::Update::configure();
    builder
        .repo_owner("xmg228")
        .repo_name("hd2-preset-helper")
        .bin_name("HD2PresetHelper")
        .current_version(env!("CARGO_PKG_VERSION"))
        .unattended();
    builder
}

fn newer_stable(candidate: &str, current: &str) -> Result<bool> {
    let candidate = Version::parse(candidate)?;
    Ok(candidate.pre.is_empty() && candidate.cmp_precedence(&Version::parse(current)?).is_gt())
}

/// Only the executable is replaced. Licenses are embedded; user data is untouched.
pub fn install(
    update: &AvailableUpdate,
    executable: &Path,
    on_install: impl Fn() + Send + Sync + 'static,
) -> Result<()> {
    ensure!(
        installation_supported(),
        "automatic installation is disabled for this build"
    );
    update.validate(env!("CARGO_PKG_VERSION"))?;
    let name = format!("HD2PresetHelper-v{}-windows-x64.zip", update.version);
    let url = update.asset_url.clone();
    let mut builder = updater();
    builder
        .release_tag(format!("v{}", update.version))
        .asset_matcher(move |assets| {
            assets
                .iter()
                .find(|asset| asset.name() == name && asset.download_url() == url)
                .cloned()
        })
        .bin_path_in_archive(BINARY_IN_ARCHIVE)
        .bin_install_path(executable)
        .timeout(Duration::from_secs(120))
        .verify_checksum(self_update::Checksum::Sha256(update.sha256.clone()))
        // The explicitly pinned checksum is mandatory, even if GitHub omits its digest later.
        .verify_release_digest(false)
        .verify_binary(move |binary| {
            if binary.metadata()?.len() == 0 {
                return Err(self_update::Error::verification_rejected(
                    "the update contains an empty executable",
                ));
            }
            on_install();
            Ok(())
        });
    let installer = builder.build()?;
    replace_with_backup(executable, || {
        installer.update()?;
        Ok(())
    })
}

fn replace_with_backup(executable: &Path, replace: impl FnOnce() -> Result<()>) -> Result<()> {
    // self-replace can fail after renaming the loaded image on Windows. Keep
    // an independent copy, since its old image is scheduled for deletion.
    let staging = tempfile::Builder::new()
        .prefix(".hd2-update-")
        .tempdir_in(
            executable
                .parent()
                .context("executable has no parent directory")?,
        )
        .context(
            "the application directory is not writable; update manually from GitHub Releases",
        )?;
    let backup = staging.path().join("previous-executable");
    fs::copy(executable, &backup).context("could not back up the installed executable")?;
    if let Err(error) = replace() {
        if !executable.exists()
            && let Err(restore) = fs::rename(&backup, executable)
        {
            let retained = staging.keep();
            return Err(error.context(format!(
                "could not restore the previous executable ({restore}); recovery files retained at {}",
                retained.display()
            )));
        }
        return Err(error);
    }
    Ok(())
}
