//! Offline equipment recognition for diagnostic builds. No game input or capture session.
use std::{
    path::PathBuf,
    time::{Instant, SystemTime, UNIX_EPOCH},
};

use anyhow::{Context, Result, bail, ensure};
use serde_json::json;

use crate::{
    app_paths::AppPaths,
    item::EquipmentKind,
    loadout::equipment::{Diagnostics, diagnostics},
    vision::equipment::EquipmentObserver,
};

pub fn run() -> Result<()> {
    let mut kind = EquipmentKind::Throwable;
    let mut source: Option<PathBuf> = None;
    let mut output: Option<PathBuf> = None;
    let mut client: Option<[u32; 4]> = None;
    let mut args = std::env::args_os().skip(2);
    while let Some(arg) = args.next() {
        if arg == "--help" {
            println!(concat!(
                "--equipment-probe --image <PNG> [--client x,y,width,height]\n",
                "  [--kind helmet|armor|cape|primary|secondary|throwable]\n",
                "  [--output <directory>]\n\n",
                "Input must be a client image or include an explicit client rectangle.\n",
                "Default kind: throwable. Results are written to a timestamped subdirectory."
            ));
            return Ok(());
        }
        let value = args
            .next()
            .with_context(|| format!("missing value for {arg:?}"))?;
        match arg.to_str() {
            Some("--kind") => {
                kind = EquipmentKind::parse(&value.to_string_lossy())
                    .context("unknown equipment kind")?;
            }
            Some("--image") => source = Some(value.into()),
            Some("--output") => output = Some(value.into()),
            Some("--client") => {
                let rect = value
                    .to_string_lossy()
                    .split(',')
                    .map(str::parse::<u32>)
                    .collect::<std::result::Result<Vec<_>, _>>()?;
                client = Some(
                    rect.try_into()
                        .map_err(|_| anyhow::anyhow!("--client requires x,y,width,height"))?,
                );
            }
            _ => bail!("unknown probe option {arg:?}"),
        }
    }
    let source = source.context("--image <PNG> is required for offline equipment inspection")?;
    let image = image::open(&source)?.into_rgba8();
    let [x, y, width, height] = client.unwrap_or([0, 0, image.width(), image.height()]);
    ensure!(
        width > 0
            && height > 0
            && x as u64 + width as u64 <= image.width() as u64
            && y as u64 + height as u64 <= image.height() as u64,
        "client rectangle lies outside the input PNG"
    );
    let root = match output {
        Some(path) => path,
        None => AppPaths::resolve()?.log.with_file_name("equipment-probe"),
    };
    let directory = root.join(
        SystemTime::now()
            .duration_since(UNIX_EPOCH)?
            .as_millis()
            .to_string(),
    );
    let _log = crate::logging::init(&directory.join("app.log"), tracing::Level::INFO)?;
    let mut output = Diagnostics::new(&directory)?;
    let (observer, resolved) = EquipmentObserver::resolve(width, height, kind)?;
    let r = resolved.rect;
    let frame = image::imageops::crop_imm(&image, x + r.x, y + r.y, r.w, r.h).to_image();
    let started = Instant::now();
    let page = observer.observe(&frame);
    let entry = observer.entry(&frame);
    let observe_ms = started.elapsed().as_secs_f64() * 1000.0;
    output.record(|| {
        json!({"event": "offline", "kind": kind, "source": source,
        "client": [x,y,width,height], "capture_rect": [r.x,r.y,r.w,r.h],
        "observe_ms": observe_ms, "page": diagnostics::describe(&page),
        "entry":entry, "entry_confirmed":entry.confirmed()})
    });
    output.save(&frame, &page, "offline");
    if entry.confirmed() {
        output.save_entry(&frame, &entry, "offline-entry");
    }
    println!("Equipment inspection output: {}", directory.display());
    Ok(())
}
