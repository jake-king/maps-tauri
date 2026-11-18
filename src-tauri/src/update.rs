//! Self-update: an installed copy asks a release manifest (`latest.json`, written by the release
//! job) whether a newer version exists, and on the user's word downloads it, checks its signature
//! against the release key, installs it and restarts.
//!
//! Where it asks is fixed when the app is built: `MAPS_UPDATE_URL`. A build without it has no
//! updater at all and makes no request; that is every development build and every test run.
//!
//! What is trusted: only the signature. The manifest is fetched over TLS but is not signed itself,
//! so the downloaded package must carry a minisign signature by the release key
//! (`release-signing.pub`, compiled in), and that signature must name the version the manifest
//! announced (`requireSignedVersion` in `tauri.conf.json`). A manifest that pairs a new version
//! number with an older, genuinely signed package is refused: that is a downgrade, not an update.
//!
//! On Linux the update is the release's `.deb`, installed with `dpkg -i` through `pkexec` (the
//! desktop's administrator prompt), as the package was installed in the first place.

use std::sync::Mutex;

use base64::Engine as _;
use serde::Serialize;
use tauri::{AppHandle, Runtime, State};
use tauri_plugin_updater::{Update, UpdaterExt};

/// The release manifest's URL, from the build's environment; `None` turns the updater off.
pub const URL: Option<&str> = option_env!("MAPS_UPDATE_URL");

/// The release key in the form the updater reads: the minisign public key file, base64-encoded.
/// `MAPS_UPDATE_PUBKEY`, at build time, puts another key's public file in its place: the
/// end-to-end check's throwaway key, since the release key's private half never leaves the forge.
fn pubkey() -> String {
    let key =
        option_env!("MAPS_UPDATE_PUBKEY").unwrap_or(include_str!("../../release-signing.pub"));
    base64::engine::general_purpose::STANDARD.encode(key)
}

/// The updater plugin, configured with the release key, when the build names a manifest.
pub fn plugin<R: Runtime>() -> Option<tauri::plugin::TauriPlugin<R, tauri_plugin_updater::Config>> {
    URL.map(|_| {
        tauri_plugin_updater::Builder::new()
            .pubkey(pubkey())
            .build()
    })
}

/// An update found by [`update_check`], kept until [`update_install`] installs it.
#[derive(Default)]
pub struct Pending(Mutex<Option<Update>>);

/// What the window shows about an available update.
#[derive(Serialize)]
pub struct Available {
    version: String,
    notes: Option<String>,
}

/// Ask the manifest for a version newer than this one.
async fn check<R: Runtime>(app: &AppHandle<R>) -> Result<Option<Update>, String> {
    let url = URL.ok_or("this build has no update URL")?;
    let url = url.parse().map_err(|e| format!("update URL {url}: {e}"))?;
    app.updater_builder()
        .endpoints(vec![url])
        .and_then(tauri_plugin_updater::UpdaterBuilder::build)
        .map_err(|e| e.to_string())?
        .check()
        .await
        .map_err(|e| e.to_string())
}

/// The newer version the manifest offers, if any (`null` when there is none or the build has no
/// update URL). The update is kept for [`update_install`].
///
/// # Errors
///
/// When the manifest cannot be fetched or read.
#[tauri::command]
pub async fn update_check<R: Runtime>(
    app: AppHandle<R>,
    pending: State<'_, Pending>,
) -> Result<Option<Available>, String> {
    if URL.is_none() {
        return Ok(None);
    }
    let update = check(&app).await?;
    let available = update.as_ref().map(|u| Available {
        version: u.version.clone(),
        notes: u.body.clone(),
    });
    *pending.0.lock().map_err(|e| e.to_string())? = update;
    Ok(available)
}

/// Download the update [`update_check`] found, check its signature, install it, and restart into
/// it. Does not return when it succeeds.
///
/// # Errors
///
/// When there is no update to install, the download fails, the signature does not verify (wrong
/// key, altered package, or a version other than the one announced), or the installer fails.
#[tauri::command]
pub async fn update_install<R: Runtime>(
    app: AppHandle<R>,
    pending: State<'_, Pending>,
) -> Result<(), String> {
    let update = pending
        .0
        .lock()
        .map_err(|e| e.to_string())?
        .take()
        .ok_or("no update to install: check first")?;
    update
        .download_and_install(|_, _| {}, || {})
        .await
        .map_err(|e| e.to_string())?;
    app.restart()
}

/// `MAPS_UPDATE=install`, for the end-to-end check (tools/update-e2e.sh): check, install what is
/// found without asking, print the outcome, and exit (0 installed or nothing newer, 1 refused).
pub fn install_now<R: Runtime>(app: AppHandle<R>) {
    tauri::async_runtime::spawn(async move {
        let outcome = match check(&app).await {
            Ok(Some(update)) => {
                let version = update.version.clone();
                match update.download_and_install(|_, _| {}, || {}).await {
                    Ok(()) => Ok(format!("installed {version}")),
                    Err(e) => Err(format!("refused {version}: {e}")),
                }
            }
            Ok(None) => Ok("nothing newer".to_string()),
            Err(e) => Err(e),
        };
        match outcome {
            Ok(msg) => {
                println!("update: {msg}");
                app.exit(0);
            }
            Err(msg) => {
                println!("update: {msg}");
                app.exit(1);
            }
        }
    });
}
