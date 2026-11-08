use anyhow::{Context, Result, ensure};
use serde_json::json;
use std::time::Instant;

use crate::capture::{CaptureRegion, CaptureSessionManager, CaptureSource, CapturedFrame};
use crate::evidence::OperationEvidence;
use crate::input::{InputSession, Key};
use crate::window::WindowTarget;

pub(crate) struct GameCapture<'a> {
    pub source: &'a mut CaptureSource,
    pub target: WindowTarget,
    pub colors: crate::game_settings::GameColorSettings,
}

/// Prepare capture after the controller has activated the requested game window.
pub(crate) fn prepare_game<'a>(
    sessions: &'a mut CaptureSessionManager,
    expected: crate::window::WindowIdentity,
) -> Result<GameCapture<'a>> {
    let target = crate::game_window::wait_for_foreground_game_window()
        .context("failed to locate Helldivers window")?;
    ensure!(
        target.identity() == expected,
        "the game window changed before the action started; try again"
    );
    crate::permissions::ensure_input_access()?;
    let colors = crate::game_settings::read_color_settings()
        .context("failed to read Helldivers color settings")?;
    let source = sessions
        .acquire(&target)
        .context("failed to get capture session")?;
    Ok(GameCapture {
        source,
        target,
        colors,
    })
}

pub struct AutomationSession<'a> {
    region: CaptureRegion<'a>,
    input: InputSession,
    evidence: &'a OperationEvidence,
    latest: Option<CapturedFrame>,
    cancellation: Option<crate::scan::ScanCancellation>,
}

impl<'a> AutomationSession<'a> {
    pub fn new(
        region: CaptureRegion<'a>,
        target: WindowTarget,
        evidence: &'a OperationEvidence,
    ) -> Result<Self> {
        Ok(Self {
            region,
            input: InputSession::new(target)?,
            evidence,
            latest: evidence.latest_frame(),
            cancellation: None,
        })
    }

    pub fn capture(&mut self) -> Result<CapturedFrame> {
        self.check_cancelled()?;
        let frame = self.region.capture()?;
        self.evidence.observe(&frame);
        self.latest = Some(frame.clone());
        Ok(frame)
    }

    pub fn evidence(&self) -> &'a OperationEvidence {
        self.evidence
    }

    pub(crate) fn set_cancellation(&mut self, cancellation: crate::scan::ScanCancellation) {
        self.cancellation = Some(cancellation);
    }

    pub(crate) fn check_cancelled(&self) -> Result<()> {
        if let Some(cancellation) = &self.cancellation {
            cancellation.check()?;
        }
        Ok(())
    }

    pub fn move_cursor(&mut self, point: (u32, u32)) -> Result<()> {
        let client = self.region.map_to_client(point);
        self.send(
            "move_cursor",
            json!({"roi_point": point, "client_point": [client.x, client.y]}),
            |input| input.move_cursor(client),
        )
    }

    pub fn click(&mut self, point: (u32, u32), hold_ms: u64) -> Result<()> {
        let client = self.region.map_to_client(point);
        self.send(
            "click",
            json!({"roi_point": point, "client_point": [client.x, client.y], "hold_ms": hold_ms}),
            |input| input.click(client, hold_ms),
        )
    }

    pub fn click_current(&mut self, hold_ms: u64) -> Result<()> {
        self.send("click_current", json!({"hold_ms": hold_ms}), |input| {
            input.click_current(hold_ms)
        })
    }

    /// Scroll in wheel notches: positive is up, negative is down.
    pub fn scroll(&mut self, notches: i32) -> Result<()> {
        self.send("scroll", json!({"notches": notches}), |input| {
            input.scroll(notches)
        })
    }

    pub fn tap_key(&mut self, key: Key, hold_ms: u64) -> Result<()> {
        self.send(
            "key",
            json!({"key": key.name(), "hold_ms": hold_ms}),
            |input| input.tap_key(key, hold_ms),
        )
    }

    fn send(
        &mut self,
        kind: &str,
        parameters: serde_json::Value,
        send: impl FnOnce(&mut InputSession) -> Result<()>,
    ) -> Result<()> {
        self.check_cancelled()?;
        let started = Instant::now();
        let result = send(&mut self.input);
        // Never encode/write images or record between injected down/up events.
        self.evidence.input(self.latest.as_ref(), json!({
            "kind": kind, "parameters": parameters, "elapsed_s": started.elapsed().as_secs_f64(),
            "sent": result.is_ok(), "error": result.as_ref().err().map(|error| format!("{error:#}")),
        }));
        result
    }
}
