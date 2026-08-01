use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use image::RgbaImage;
use tracing::{debug, warn};
use windows::Foundation::TypedEventHandler;
use windows::Graphics::Capture::{
    Direct3D11CaptureFrame, Direct3D11CaptureFramePool, GraphicsCaptureAccess,
    GraphicsCaptureAccessKind, GraphicsCaptureItem, GraphicsCaptureSession,
};
use windows::Graphics::DirectX::Direct3D11::{IDirect3DDevice, IDirect3DSurface};
use windows::Graphics::DirectX::DirectXPixelFormat;
use windows::Graphics::SizeInt32;
use windows::Security::Authorization::AppCapabilityAccess::AppCapabilityAccessStatus;
use windows::Win32::Foundation::{ERROR_SUCCESS, HMODULE, HWND, LUID, WIN32_ERROR};
use windows::Win32::Graphics::Direct3D::D3D_DRIVER_TYPE_HARDWARE;
use windows::Win32::Graphics::Direct3D11::{
    D3D11_BOX, D3D11_CPU_ACCESS_READ, D3D11_CREATE_DEVICE_BGRA_SUPPORT, D3D11_MAP_READ,
    D3D11_MAPPED_SUBRESOURCE, D3D11_SDK_VERSION, D3D11_TEXTURE2D_DESC, D3D11_USAGE_STAGING,
    D3D11CreateDevice, ID3D11Device, ID3D11DeviceContext, ID3D11Resource, ID3D11Texture2D,
};
use windows::Win32::Graphics::Dxgi::Common::{
    DXGI_FORMAT, DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_FORMAT_R16G16B16A16_FLOAT, DXGI_SAMPLE_DESC,
};
use windows::Win32::Graphics::Dxgi::IDXGIDevice;
use windows::Win32::Graphics::Gdi::{
    GetMonitorInfoW, MONITOR_DEFAULTTONEAREST, MONITORINFOEXW, MonitorFromWindow,
};
use windows::Win32::System::WinRT::Direct3D11::{
    CreateDirect3D11DeviceFromDXGIDevice, IDirect3DDxgiInterfaceAccess,
};
use windows::Win32::System::WinRT::Graphics::Capture::IGraphicsCaptureItemInterop;
use windows::Win32::System::WinRT::{RO_INIT_MULTITHREADED, RoInitialize};
use windows::Win32::UI::WindowsAndMessaging::IsWindow;
use windows::core::{IInspectable, Interface, factory};

use half::f16;
use windows::Win32::Devices::Display::{
    DISPLAYCONFIG_DEVICE_INFO_GET_ADVANCED_COLOR_INFO,
    DISPLAYCONFIG_DEVICE_INFO_GET_SDR_WHITE_LEVEL, DISPLAYCONFIG_DEVICE_INFO_GET_SOURCE_NAME,
    DISPLAYCONFIG_GET_ADVANCED_COLOR_INFO, DISPLAYCONFIG_MODE_INFO, DISPLAYCONFIG_PATH_INFO,
    DISPLAYCONFIG_SDR_WHITE_LEVEL, DISPLAYCONFIG_SOURCE_DEVICE_NAME, DisplayConfigGetDeviceInfo,
    GetDisplayConfigBufferSizes, QDC_ONLY_ACTIVE_PATHS, QueryDisplayConfig,
};
use windows::core::HRESULT;

use crate::game_window::{ClientCrop, GameWindow};
use crate::vision::{Rect, RoiFrame};

const WGC_FRAME_POOL_BUFFER_COUNT: i32 = 2;

pub struct CaptureSource {
    screen_x: i32,
    screen_y: i32,
    rebuild_signature: CaptureRebuildSignature,
    device: ID3D11Device,
    context: ID3D11DeviceContext,
    _direct3d_device: IDirect3DDevice,
    frame_pool: Direct3D11CaptureFramePool,
    frame_arrived_token: i64,
    frame_bus: WgcFrameBus,
    consumed_generation: u64,
    session: GraphicsCaptureSession,
    item_size: SizeInt32,
    client_crop: ClientCrop,

    texture_read: TextureReadCache,
}

const WGC_FRAME_WAIT_TIMEOUT: Duration = Duration::from_millis(500);

struct WgcFrameBus {
    shared: Arc<(Mutex<WgcFrameBusState>, Condvar)>,
}

struct WgcFrameBusState {
    generation: u64,
    latest: Option<Direct3D11CaptureFrame>,
    published_at: Option<Instant>,
    closed: bool,
}

impl WgcFrameBus {
    fn register(frame_pool: &Direct3D11CaptureFramePool) -> Result<(Self, i64)> {
        let shared = Arc::new((
            Mutex::new(WgcFrameBusState {
                generation: 0,
                latest: None,
                published_at: None,
                closed: false,
            }),
            Condvar::new(),
        ));
        let handler_shared = Arc::clone(&shared);

        let token = frame_pool
            .FrameArrived(
                &TypedEventHandler::<Direct3D11CaptureFramePool, IInspectable>::new(
                    move |sender, _| {
                        let Some(sender) = sender.as_ref() else {
                            return Ok(());
                        };

                        // Keep only the newest queued frame. Intermediate frames are obsolete for
                        // recognition, but they must be closed promptly to return their pool buffers.
                        let mut newest = None;
                        let mut received = 0u64;
                        while let Ok(frame) = sender.TryGetNextFrame() {
                            received = received.saturating_add(1);
                            if let Some(old) = newest.replace(frame) {
                                let _ = old.Close();
                            }
                        }

                        let Some(frame) = newest else {
                            return Ok(());
                        };

                        let (state_lock, changed) = &*handler_shared;
                        let mut state = state_lock
                            .lock()
                            .unwrap_or_else(|poisoned| poisoned.into_inner());
                        if state.closed {
                            drop(state);
                            let _ = frame.Close();
                            return Ok(());
                        }

                        let old = state.latest.replace(frame);
                        state.generation = state.generation.saturating_add(received);
                        state.published_at = Some(Instant::now());
                        drop(state);
                        changed.notify_all();

                        // Closing a replaced frame can touch WinRT/D3D internals. Keep that work
                        // outside the bus mutex and after notification so a waiting consumer can
                        // start immediately.
                        if let Some(old) = old {
                            let _ = old.Close();
                        }
                        Ok(())
                    },
                ),
            )
            .context("failed to register WGC FrameArrived handler")?;

        Ok((Self { shared }, token))
    }

    fn sync_generation(&self) -> u64 {
        let (state_lock, _) = &*self.shared;
        state_lock
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .generation
    }

    fn close(&self) {
        let (state_lock, changed) = &*self.shared;
        let mut state = state_lock
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        state.closed = true;
        let frame = state.latest.take();
        state.published_at = None;
        drop(state);

        if let Some(frame) = frame {
            let _ = frame.Close();
        }
        changed.notify_all();
    }
}

impl Drop for WgcFrameBus {
    fn drop(&mut self) {
        self.close();
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct StagingKey {
    width: u32,
    height: u32,
    format: DXGI_FORMAT,
}

struct TextureReadCache {
    staging_texture: Option<ID3D11Texture2D>,
    staging_key: Option<StagingKey>,
    sdr_white_level: u32,
    f16_to_sdr_u8_lut: Box<[u8; 65536]>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct DisplayWhiteLevel {
    pub(crate) device: String,
    pub(crate) advanced_color: bool,
    pub(crate) value: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CaptureRebuildSignature {
    hwnd: isize,
    client_w: u32,
    client_h: u32,
    crop_x: u32,
    crop_y: u32,
    crop_w: u32,
    crop_h: u32,
}

impl CaptureRebuildSignature {
    pub fn from_game_window(game_window: GameWindow) -> Result<Self> {
        let crop = game_window.client_crop_in_frame()?;
        Ok(Self::from_game_window_and_crop(game_window, crop))
    }

    fn from_game_window_and_crop(game_window: GameWindow, crop: ClientCrop) -> Self {
        Self {
            hwnd: game_window.hwnd.0 as isize,
            client_w: game_window.client_w,
            client_h: game_window.client_h,
            crop_x: crop.x,
            crop_y: crop.y,
            crop_w: crop.w,
            crop_h: crop.h,
        }
    }
}

impl CaptureSource {
    pub fn new_for_game_window(game_window: GameWindow, sdr_white_level: u32) -> Result<Self> {
        let _ = unsafe { RoInitialize(RO_INIT_MULTITHREADED) };

        if !GraphicsCaptureSession::IsSupported().context("failed to query WGC support")? {
            bail!("Windows Graphics Capture is not supported on this system");
        }

        let item = wgc_item_for_window(game_window.hwnd)?;
        let item_size = item.Size().context("failed to query WGC item size")?;
        if item_size.Width <= 0 || item_size.Height <= 0 {
            bail!(
                "invalid WGC item size: {}x{}",
                item_size.Width,
                item_size.Height
            );
        }
        let crop = game_window.client_crop_in_frame()?;
        let rebuild_signature =
            CaptureRebuildSignature::from_game_window_and_crop(game_window, crop);
        if crop.x + crop.w > item_size.Width as u32 || crop.y + crop.h > item_size.Height as u32 {
            bail!(
                "game client crop ({},{},{},{}) is outside WGC frame {}x{}",
                crop.x,
                crop.y,
                crop.w,
                crop.h,
                item_size.Width,
                item_size.Height
            );
        }
        let screen_x = game_window.client_x;
        let screen_y = game_window.client_y;
        debug!(
            screen_x,
            screen_y,
            item_w = item_size.Width,
            item_h = item_size.Height,
            output_w = crop.w,
            output_h = crop.h,
            "configured WGC capture target"
        );

        let (device, context) = create_d3d11_device()?;
        let direct3d_device = create_direct3d_device_from_d3d11(&device)?;
        let frame_pool = Direct3D11CaptureFramePool::CreateFreeThreaded(
            &direct3d_device,
            DirectXPixelFormat::R16G16B16A16Float,
            WGC_FRAME_POOL_BUFFER_COUNT,
            item_size,
        )
        .context("failed to create WGC frame pool")?;
        let (frame_bus, frame_arrived_token) = WgcFrameBus::register(&frame_pool)?;
        let session = frame_pool
            .CreateCaptureSession(&item)
            .context("failed to create WGC session")?;
        let _ = session.SetIsCursorCaptureEnabled(false);
        if request_wgc_borderless_access() {
            if let Err(error) = session.SetIsBorderRequired(false) {
                warn!(
                    error = ?error,
                    "WGC borderless access was allowed but SetIsBorderRequired(false) failed"
                );
            }
        } else {
            debug!("WGC borderless access is unavailable; the yellow border may remain visible");
        }
        session
            .StartCapture()
            .context("failed to start WGC capture")?;
        let sdr_white_level = sdr_white_level.max(1);
        debug!(
            sdr_white_level,
            scale = 1000.0 / sdr_white_level as f32,
            "configured WGC HDR to SDR mapping"
        );
        let f16_to_sdr_u8_lut = build_f16_to_sdr_u8_lut(sdr_white_level);
        Ok(Self {
            screen_x,
            screen_y,
            rebuild_signature,
            device,
            context,
            _direct3d_device: direct3d_device,
            frame_pool,
            frame_arrived_token,
            frame_bus,
            consumed_generation: 0,
            session,
            item_size,
            client_crop: crop,
            texture_read: TextureReadCache {
                staging_texture: None,
                staging_key: None,
                sdr_white_level,
                f16_to_sdr_u8_lut,
            },
        })
    }

    pub fn is_capture_window_alive(&self) -> bool {
        let hwnd = HWND(self.rebuild_signature.hwnd as _);
        unsafe { IsWindow(Some(hwnd)).as_bool() }
    }

    pub fn try_reuse_for_game_window(
        &mut self,
        game_window: GameWindow,
        sdr_white_level: Option<u32>,
    ) -> bool {
        if !self.is_capture_window_alive()
            || CaptureRebuildSignature::from_game_window(game_window)
                .map(|signature| signature != self.rebuild_signature)
                .unwrap_or(true)
        {
            return false;
        }
        if let Some(sdr_white_level) = sdr_white_level {
            self.update_sdr_white_level(sdr_white_level);
        }
        self.screen_x = game_window.client_x;
        self.screen_y = game_window.client_y;
        true
    }

    fn update_sdr_white_level(&mut self, sdr_white_level: u32) {
        let sdr_white_level = sdr_white_level.max(1);
        let previous = self.texture_read.sdr_white_level;
        if sdr_white_level == previous {
            return;
        }

        self.texture_read.f16_to_sdr_u8_lut = build_f16_to_sdr_u8_lut(sdr_white_level);
        self.texture_read.sdr_white_level = sdr_white_level;
        debug!(
            previous_sdr_white_level = previous,
            sdr_white_level,
            scale = 1000.0 / sdr_white_level as f32,
            "updated WGC HDR to SDR mapping"
        );
    }

    pub fn output_size(&self) -> (u32, u32) {
        (self.client_crop.w, self.client_crop.h)
    }

    pub fn sync_to_latest(&mut self) {
        self.consumed_generation = self.frame_bus.sync_generation();
    }

    pub fn capture_latest_region(&mut self, client_roi: Rect) -> Result<RoiFrame> {
        if client_roi.w == 0 || client_roi.h == 0 {
            bail!("cannot capture an empty WGC client region");
        }
        if client_roi.x + client_roi.w > self.client_crop.w
            || client_roi.y + client_roi.h > self.client_crop.h
        {
            bail!(
                "client ROI ({},{},{},{}) is outside WGC client crop {}x{}",
                client_roi.x,
                client_roi.y,
                client_roi.w,
                client_roi.h,
                self.client_crop.w,
                self.client_crop.h
            );
        }

        let wait_start = Instant::now();
        let shared = Arc::clone(&self.frame_bus.shared);
        let (state_lock, changed) = &*shared;
        let mut state = state_lock
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());

        loop {
            if state.closed {
                bail!("WGC frame bus closed while waiting for a frame");
            }
            if state.generation > self.consumed_generation && state.latest.is_some() {
                break;
            }

            let Some(remaining) = WGC_FRAME_WAIT_TIMEOUT.checked_sub(wait_start.elapsed()) else {
                bail!("timed out waiting for an unseen WGC frame");
            };
            let (next_state, wait_result) = changed
                .wait_timeout(state, remaining)
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            state = next_state;
            if wait_result.timed_out()
                && !(state.generation > self.consumed_generation && state.latest.is_some())
            {
                bail!("timed out waiting for an unseen WGC frame");
            }
        }

        let generation = state.generation;
        let previous_generation = self.consumed_generation;
        let skipped_generations = if previous_generation == 0 {
            0
        } else {
            generation
                .saturating_sub(previous_generation)
                .saturating_sub(1)
        };
        let wait_elapsed = wait_start.elapsed();
        let frame_age = state
            .published_at
            .map(|published_at| published_at.elapsed())
            .unwrap_or_default();
        // Take ownership of the published frame while holding the mutex, then release the
        // bus immediately. The FrameArrived callback can now publish frame N+1 while the main
        // thread performs the D3D copy, map and FP16-to-RGBA conversion for frame N.
        let frame = state.latest.take().expect("checked latest WGC frame above");
        state.published_at = None;
        drop(state);

        let read_start = Instant::now();
        let read_result = self.read_client_region(&frame, client_roi);
        let read_elapsed = read_start.elapsed();
        let _ = frame.Close();
        let captured = read_result?;
        self.consumed_generation = generation;

        let generation_after_read = self.frame_bus.sync_generation();
        let newer_frame_published = generation_after_read > generation;
        debug!(
            target: "hd2_preset_helper::perf",
            generation,
            wait = ?wait_elapsed,
            age = ?frame_age,
            read = ?read_elapsed,
            newer_frame_published,
            skipped_generations,
            roi_x = client_roi.x,
            roi_y = client_roi.y,
            roi_w = client_roi.w,
            roi_h = client_roi.h,
            "WGC frame timing"
        );

        Ok(captured)
    }

    fn read_client_region(
        &mut self,
        frame: &Direct3D11CaptureFrame,
        client_roi: Rect,
    ) -> Result<RoiFrame> {
        let content_size = frame.ContentSize().unwrap_or(self.item_size);
        let surface = frame.Surface().context("failed to get WGC frame surface")?;
        let texture = d3d11_texture_from_surface(&surface)?;
        let source_region = Rect {
            x: self.client_crop.x + client_roi.x,
            y: self.client_crop.y + client_roi.y,
            w: client_roi.w,
            h: client_roi.h,
        };

        let image = read_d3d11_texture_region_to_rgba_cached(
            &self.device,
            &self.context,
            &texture,
            content_size,
            source_region,
            &mut self.texture_read,
        )?;

        Ok(RoiFrame {
            image,
            screen_x: self.screen_x + client_roi.x as i32,
            screen_y: self.screen_y + client_roi.y as i32,
        })
    }
}

impl Drop for CaptureSource {
    fn drop(&mut self) {
        let _ = self.session.Close();
        let _ = self.frame_pool.RemoveFrameArrived(self.frame_arrived_token);
        self.frame_bus.close();
        let _ = self.frame_pool.Close();
    }
}

fn create_d3d11_device() -> Result<(ID3D11Device, ID3D11DeviceContext)> {
    let mut device = None;
    let mut context = None;

    unsafe {
        D3D11CreateDevice(
            None,
            D3D_DRIVER_TYPE_HARDWARE,
            HMODULE::default(),
            D3D11_CREATE_DEVICE_BGRA_SUPPORT,
            None,
            D3D11_SDK_VERSION,
            Some(&mut device),
            None,
            Some(&mut context),
        )
    }
    .context("failed to create D3D11 device")?;

    Ok((
        device.context("D3D11CreateDevice returned no device")?,
        context.context("D3D11CreateDevice returned no immediate context")?,
    ))
}

fn create_direct3d_device_from_d3d11(device: &ID3D11Device) -> Result<IDirect3DDevice> {
    let dxgi_device: IDXGIDevice = device
        .cast()
        .context("failed to cast D3D11 device to IDXGIDevice")?;

    let inspectable: IInspectable = unsafe { CreateDirect3D11DeviceFromDXGIDevice(&dxgi_device) }
        .context("failed to create WinRT Direct3D device")?;

    inspectable
        .cast()
        .context("failed to cast WinRT object to IDirect3DDevice")
}

fn read_d3d11_texture_region_to_rgba_cached(
    device: &ID3D11Device,
    context: &ID3D11DeviceContext,
    texture: &ID3D11Texture2D,
    content_size: SizeInt32,
    region: Rect,
    cache: &mut TextureReadCache,
) -> Result<RgbaImage> {
    let t0 = Instant::now();

    let mut src_desc = D3D11_TEXTURE2D_DESC::default();
    unsafe { texture.GetDesc(&mut src_desc) };

    let format = src_desc.Format;
    if format != DXGI_FORMAT_B8G8R8A8_UNORM && format != DXGI_FORMAT_R16G16B16A16_FLOAT {
        bail!("unsupported D3D11 texture format: {:?}", format);
    }

    let texture_width = src_desc.Width.max(1);
    let texture_height = src_desc.Height.max(1);
    let content_width = (content_size.Width.max(1) as u32).min(texture_width);
    let content_height = (content_size.Height.max(1) as u32).min(texture_height);

    if region.w == 0 || region.h == 0 {
        bail!("cannot read an empty D3D11 texture region");
    }
    if region.x + region.w > content_width || region.y + region.h > content_height {
        bail!(
            "D3D11 read region ({},{},{},{}) is outside content {}x{} / texture {}x{}",
            region.x,
            region.y,
            region.w,
            region.h,
            content_width,
            content_height,
            texture_width,
            texture_height
        );
    }

    let key = StagingKey {
        width: region.w,
        height: region.h,
        format,
    };

    let t = Instant::now();
    if cache.staging_texture.is_none() || cache.staging_key != Some(key) {
        let mut staging_desc = src_desc;
        staging_desc.Width = region.w;
        staging_desc.Height = region.h;
        staging_desc.MipLevels = 1;
        staging_desc.ArraySize = 1;
        staging_desc.SampleDesc = DXGI_SAMPLE_DESC {
            Count: 1,
            Quality: 0,
        };
        staging_desc.Usage = D3D11_USAGE_STAGING;
        staging_desc.BindFlags = 0;
        staging_desc.CPUAccessFlags = D3D11_CPU_ACCESS_READ.0 as u32;
        staging_desc.MiscFlags = 0;

        let mut staging = None;
        unsafe { device.CreateTexture2D(&staging_desc, None, Some(&mut staging)) }
            .context("failed to create CPU-readable D3D11 staging texture")?;
        cache.staging_texture =
            Some(staging.context("CreateTexture2D returned no staging texture")?);
        cache.staging_key = Some(key);
    }
    let t_staging = t.elapsed();

    let staging = cache
        .staging_texture
        .as_ref()
        .context("missing cached staging texture")?;

    let dst: ID3D11Resource = staging
        .cast()
        .context("failed to cast staging texture to resource")?;
    let src: ID3D11Resource = texture
        .cast()
        .context("failed to cast source texture to resource")?;

    let source_box = D3D11_BOX {
        left: region.x,
        top: region.y,
        front: 0,
        right: region.x + region.w,
        bottom: region.y + region.h,
        back: 1,
    };

    unsafe {
        context.CopySubresourceRegion(&dst, 0, 0, 0, 0, &src, 0, Some(&source_box));
    }

    let t = Instant::now();
    let mut mapped = D3D11_MAPPED_SUBRESOURCE::default();
    unsafe { context.Map(&dst, 0, D3D11_MAP_READ, 0, Some(&mut mapped)) }
        .context("failed to map D3D11 staging texture")?;
    let t_map = t.elapsed();

    let t = Instant::now();
    let width = region.w;
    let height = region.h;
    let mut rgba = vec![0u8; width as usize * height as usize * 4];
    let t_alloc = t.elapsed();

    let row_pitch = mapped.RowPitch as usize;
    let source = mapped.pData as *const u8;

    let t = Instant::now();
    for row in 0..height as usize {
        let row_ptr = unsafe { source.add(row * row_pitch) };
        let dst_row = &mut rgba[row * width as usize * 4..(row + 1) * width as usize * 4];

        match format {
            DXGI_FORMAT_B8G8R8A8_UNORM => {
                let row_bytes = unsafe { std::slice::from_raw_parts(row_ptr, width as usize * 4) };
                for (src_px, dst_px) in row_bytes.chunks_exact(4).zip(dst_row.chunks_exact_mut(4)) {
                    dst_px[0] = src_px[2];
                    dst_px[1] = src_px[1];
                    dst_px[2] = src_px[0];
                    dst_px[3] = 255;
                }
            }

            DXGI_FORMAT_R16G16B16A16_FLOAT => {
                let row_bytes = unsafe { std::slice::from_raw_parts(row_ptr, width as usize * 8) };
                read_rgba16f_row_to_rgba8_slice(row_bytes, dst_row, &cache.f16_to_sdr_u8_lut);
            }

            _ => unreachable!(),
        }
    }
    let t_convert = t.elapsed();

    unsafe { context.Unmap(&dst, 0) };

    let image = RgbaImage::from_raw(width, height, rgba)
        .context("failed to build RGBA image from D3D11 texture")?;

    let total = t0.elapsed();
    debug!(
        target: "hd2_preset_helper::perf",
        total = ?total,
        staging = ?t_staging,
        mapping = ?t_map,
        allocation = ?t_alloc,
        conversion = ?t_convert,
        texture_w = texture_width,
        texture_h = texture_height,
        region_x = region.x,
        region_y = region.y,
        region_w = region.w,
        region_h = region.h,
        "read D3D11 texture to RGBA"
    );
    Ok(image)
}

fn read_rgba16f_row_to_rgba8_slice(row_bytes: &[u8], rgba: &mut [u8], lut: &[u8; 65536]) {
    for (pixel, dst) in row_bytes.chunks_exact(8).zip(rgba.chunks_exact_mut(4)) {
        let r = u16::from_le_bytes([pixel[0], pixel[1]]);
        let g = u16::from_le_bytes([pixel[2], pixel[3]]);
        let b = u16::from_le_bytes([pixel[4], pixel[5]]);

        dst[0] = lut[r as usize];
        dst[1] = lut[g as usize];
        dst[2] = lut[b as usize];
        dst[3] = 255;
    }
}

fn linear_to_srgb_u8(x: f32) -> u8 {
    let x = x.clamp(0.0, 1.0);

    let y = if x <= 0.003_130_8 {
        12.92 * x
    } else {
        1.055 * x.powf(1.0 / 2.4) - 0.055
    };

    (y * 255.0 + 0.5).clamp(0.0, 255.0) as u8
}

fn build_f16_to_sdr_u8_lut(sdr_white_level: u32) -> Box<[u8; 65536]> {
    let sdr_white_level = sdr_white_level.max(1);
    let scale = 1000.0 / sdr_white_level as f32;

    let mut lut = Box::new([0u8; 65536]);

    for bits in 0u32..=65535 {
        let x = f16::from_bits(bits as u16).to_f32();

        let y = if x.is_finite() { x * scale } else { 0.0 };

        lut[bits as usize] = linear_to_srgb_u8(y);
    }

    lut
}

fn request_wgc_borderless_access() -> bool {
    let operation =
        match GraphicsCaptureAccess::RequestAccessAsync(GraphicsCaptureAccessKind::Borderless) {
            Ok(operation) => operation,
            Err(error) => {
                debug!(error = ?error, "failed to request WGC borderless access");
                return false;
            }
        };

    match operation.GetResults() {
        Ok(AppCapabilityAccessStatus::Allowed) => true,
        Ok(status) => {
            debug!(status = ?status, "WGC borderless access denied");
            false
        }
        Err(error) => {
            debug!(
                error = ?error,
                "failed while waiting for WGC borderless access result"
            );
            false
        }
    }
}

fn d3d11_texture_from_surface(surface: &IDirect3DSurface) -> Result<ID3D11Texture2D> {
    let access: IDirect3DDxgiInterfaceAccess = surface
        .cast()
        .context("failed to access DXGI interface from WGC surface")?;
    unsafe { access.GetInterface::<ID3D11Texture2D>() }
        .context("failed to get D3D11 texture from WGC surface")
}

fn wgc_item_for_window(hwnd: windows::Win32::Foundation::HWND) -> Result<GraphicsCaptureItem> {
    let interop: IGraphicsCaptureItemInterop =
        factory::<GraphicsCaptureItem, IGraphicsCaptureItemInterop>()
            .context("failed to get GraphicsCaptureItem interop factory")?;
    unsafe { interop.CreateForWindow(hwnd) }.context("failed to create WGC item for window")
}

pub(crate) fn query_sdr_white_level_for_window(hwnd: HWND) -> Result<DisplayWhiteLevel> {
    unsafe {
        let monitor = MonitorFromWindow(hwnd, MONITOR_DEFAULTTONEAREST);
        if monitor.is_invalid() {
            bail!("failed to resolve the monitor containing the game window");
        }

        let mut monitor_info = MONITORINFOEXW::default();
        monitor_info.monitorInfo.cbSize = std::mem::size_of::<MONITORINFOEXW>() as u32;
        if !GetMonitorInfoW(monitor, &mut monitor_info.monitorInfo).as_bool() {
            bail!("GetMonitorInfoW failed for the game window monitor");
        }
        let monitor_device = utf16_trim(&monitor_info.szDevice);

        let mut path_count = 0u32;
        let mut mode_count = 0u32;
        let error =
            GetDisplayConfigBufferSizes(QDC_ONLY_ACTIVE_PATHS, &mut path_count, &mut mode_count);
        win32_error_to_result(error, "GetDisplayConfigBufferSizes failed")?;

        let mut paths = vec![DISPLAYCONFIG_PATH_INFO::default(); path_count as usize];
        let mut modes = vec![DISPLAYCONFIG_MODE_INFO::default(); mode_count as usize];
        let error = QueryDisplayConfig(
            QDC_ONLY_ACTIVE_PATHS,
            &mut path_count,
            paths.as_mut_ptr(),
            &mut mode_count,
            modes.as_mut_ptr(),
            None,
        );
        win32_error_to_result(error, "QueryDisplayConfig failed")?;
        paths.truncate(path_count as usize);

        for path in paths {
            let Ok(source_name) = query_source_name(path.sourceInfo.adapterId, path.sourceInfo.id)
            else {
                continue;
            };
            if !source_name.eq_ignore_ascii_case(&monitor_device) {
                continue;
            }

            let adapter_id = path.targetInfo.adapterId;
            let target_id = path.targetInfo.id;
            let advanced_color = query_advanced_color_enabled(adapter_id, target_id)?;
            let value = if advanced_color {
                query_sdr_white_level(adapter_id, target_id)?.max(1)
            } else {
                1000
            };

            return Ok(DisplayWhiteLevel {
                device: monitor_device,
                advanced_color,
                value,
            });
        }

        bail!("no active DisplayConfig path matches game window monitor {monitor_device:?}")
    }
}

unsafe fn query_source_name(adapter_id: LUID, source_id: u32) -> Result<String> {
    let mut name = DISPLAYCONFIG_SOURCE_DEVICE_NAME::default();
    name.header.r#type = DISPLAYCONFIG_DEVICE_INFO_GET_SOURCE_NAME;
    name.header.size = std::mem::size_of::<DISPLAYCONFIG_SOURCE_DEVICE_NAME>() as u32;
    name.header.adapterId = adapter_id;
    name.header.id = source_id;

    let error = unsafe { DisplayConfigGetDeviceInfo(&mut name.header) };
    win32_i32_to_result(error, "DisplayConfigGetDeviceInfo(GET_SOURCE_NAME) failed")?;
    Ok(utf16_trim(&name.viewGdiDeviceName))
}

unsafe fn query_sdr_white_level(adapter_id: LUID, target_id: u32) -> Result<u32> {
    let mut white = DISPLAYCONFIG_SDR_WHITE_LEVEL::default();

    white.header.r#type = DISPLAYCONFIG_DEVICE_INFO_GET_SDR_WHITE_LEVEL;
    white.header.size = std::mem::size_of::<DISPLAYCONFIG_SDR_WHITE_LEVEL>() as u32;
    white.header.adapterId = adapter_id;
    white.header.id = target_id;

    let err_code = unsafe { DisplayConfigGetDeviceInfo(&mut white.header) };
    win32_i32_to_result(
        err_code,
        "DisplayConfigGetDeviceInfo(GET_SDR_WHITE_LEVEL) failed",
    )?;

    Ok(white.SDRWhiteLevel)
}

unsafe fn query_advanced_color_enabled(adapter_id: LUID, target_id: u32) -> Result<bool> {
    let mut info = DISPLAYCONFIG_GET_ADVANCED_COLOR_INFO::default();

    info.header.r#type = DISPLAYCONFIG_DEVICE_INFO_GET_ADVANCED_COLOR_INFO;
    info.header.size = std::mem::size_of::<DISPLAYCONFIG_GET_ADVANCED_COLOR_INFO>() as u32;
    info.header.adapterId = adapter_id;
    info.header.id = target_id;

    let err_code = unsafe { DisplayConfigGetDeviceInfo(&mut info.header) };
    win32_i32_to_result(
        err_code,
        "DisplayConfigGetDeviceInfo(GET_ADVANCED_COLOR_INFO) failed",
    )?;

    let value = unsafe { info.Anonymous.value };
    Ok((value & 0x2) != 0)
}

fn win32_error_to_result(error: WIN32_ERROR, message: &'static str) -> Result<()> {
    if error == ERROR_SUCCESS {
        Ok(())
    } else {
        Err(anyhow::anyhow!(
            "{}: WIN32_ERROR({}) / HRESULT({:#010x})",
            message,
            error.0,
            HRESULT::from_win32(error.0).0 as u32,
        ))
    }
}
fn win32_i32_to_result(code: i32, message: &'static str) -> Result<()> {
    if code == 0 {
        Ok(())
    } else {
        bail!(
            "{}: code={} / HRESULT({:#010x})",
            message,
            code,
            HRESULT::from_win32(code as u32).0 as u32,
        )
    }
}

fn utf16_trim(s: &[u16]) -> String {
    let len = s.iter().position(|c| *c == 0).unwrap_or(s.len());
    String::from_utf16_lossy(&s[..len])
}
