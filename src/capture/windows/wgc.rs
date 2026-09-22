use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};
#[cfg(feature = "diagnostics")]
use std::{
    io::{BufWriter, Write},
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

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
use windows::Win32::Foundation::{HMODULE, HWND};
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
use windows::Win32::System::WinRT::Direct3D11::{
    CreateDirect3D11DeviceFromDXGIDevice, IDirect3DDxgiInterfaceAccess,
};
use windows::Win32::System::WinRT::Graphics::Capture::IGraphicsCaptureItemInterop;
use windows::Win32::System::WinRT::{RO_INIT_MULTITHREADED, RoInitialize};
use windows::Win32::UI::WindowsAndMessaging::IsWindow;
use windows::core::{IInspectable, Interface, factory};

use super::{CapturePixelFormat, ColorNormalizer};
use crate::image_rect::ImageRect;
use crate::window::{ClientCrop, ClientPoint, WindowTarget};

const WGC_FRAME_POOL_BUFFER_COUNT: i32 = 2;
#[cfg(feature = "diagnostics")]
const WGC_F16_DUMP_DIR_ENV: &str = "HD2_PRESET_HELPER_WGC_F16_DUMP_DIR";

fn directx_pixel_format(format: CapturePixelFormat) -> DirectXPixelFormat {
    match format {
        CapturePixelFormat::Bgra8 => DirectXPixelFormat::B8G8R8A8UIntNormalized,
        CapturePixelFormat::Rgba16Float => DirectXPixelFormat::R16G16B16A16Float,
    }
}

pub(super) struct WgcCapture {
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

    fn generation(&self) -> u64 {
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
    #[cfg(feature = "diagnostics")]
    f16_dump_dir: Option<PathBuf>,
    #[cfg(feature = "diagnostics")]
    f16_dumped: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct CaptureRebuildSignature {
    hwnd: isize,
    client_w: u32,
    client_h: u32,
    crop_x: u32,
    crop_y: u32,
    crop_w: u32,
    crop_h: u32,
}

impl CaptureRebuildSignature {
    fn from_window_target(target: &WindowTarget) -> Result<Self> {
        let crop = target.client_crop_in_frame()?;
        Ok(Self::from_window_target_and_crop(target, crop))
    }

    fn from_window_target_and_crop(target: &WindowTarget, crop: ClientCrop) -> Self {
        let (client_w, client_h) = target.client_size();
        Self {
            hwnd: target.native_handle().0 as isize,
            client_w,
            client_h,
            crop_x: crop.x,
            crop_y: crop.y,
            crop_w: crop.w,
            crop_h: crop.h,
        }
    }
}

impl WgcCapture {
    pub(super) fn map_to_client(&self, roi: ImageRect, local: (u32, u32)) -> ClientPoint {
        // WGC output is cropped to the client area without resampling. ROI and
        // input therefore share its pixel origin and scale; DWM borders are
        // already removed by client_crop, not added to input coordinates.
        ClientPoint {
            x: f64::from(roi.x) + f64::from(local.0),
            y: f64::from(roi.y) + f64::from(local.1),
        }
    }

    pub(super) fn new(target: &WindowTarget, pixel_format: CapturePixelFormat) -> Result<Self> {
        let _ = unsafe { RoInitialize(RO_INIT_MULTITHREADED) };

        if !GraphicsCaptureSession::IsSupported().context("failed to query WGC support")? {
            bail!("Windows Graphics Capture is not supported on this system");
        }

        let item = wgc_item_for_window(target.native_handle())?;
        let item_size = item.Size().context("failed to query WGC item size")?;
        if item_size.Width <= 0 || item_size.Height <= 0 {
            bail!(
                "invalid WGC item size: {}x{}",
                item_size.Width,
                item_size.Height
            );
        }
        let crop = target.client_crop_in_frame()?;
        let rebuild_signature = CaptureRebuildSignature::from_window_target_and_crop(target, crop);
        if crop.x + crop.w > item_size.Width as u32 || crop.y + crop.h > item_size.Height as u32 {
            bail!(
                "window client crop ({},{},{},{}) is outside WGC frame {}x{}",
                crop.x,
                crop.y,
                crop.w,
                crop.h,
                item_size.Width,
                item_size.Height
            );
        }
        let (screen_x, screen_y) = target.client_origin();
        debug!(
            screen_x,
            screen_y,
            item_w = item_size.Width,
            item_h = item_size.Height,
            output_w = crop.w,
            output_h = crop.h,
            wgc_format = pixel_format.label(),
            "configured WGC capture target"
        );

        let (device, context) = create_d3d11_device()?;
        let direct3d_device = create_direct3d_device_from_d3d11(&device)?;
        let frame_pool = Direct3D11CaptureFramePool::CreateFreeThreaded(
            &direct3d_device,
            directx_pixel_format(pixel_format),
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
        Ok(Self {
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
                #[cfg(feature = "diagnostics")]
                f16_dump_dir: std::env::var_os(WGC_F16_DUMP_DIR_ENV).map(PathBuf::from),
                #[cfg(feature = "diagnostics")]
                f16_dumped: false,
            },
        })
    }

    fn is_capture_window_alive(&self) -> bool {
        let hwnd = HWND(self.rebuild_signature.hwnd as _);
        unsafe { IsWindow(Some(hwnd)).as_bool() }
    }

    pub(super) fn try_reuse(&mut self, target: &WindowTarget) -> bool {
        if !self.is_capture_window_alive()
            || CaptureRebuildSignature::from_window_target(target)
                .map(|signature| signature != self.rebuild_signature)
                .unwrap_or(true)
        {
            return false;
        }
        true
    }

    pub(super) fn output_size(&self) -> (u32, u32) {
        (self.client_crop.w, self.client_crop.h)
    }

    pub(super) fn capture_region(
        &mut self,
        client_roi: ImageRect,
        converter: &ColorNormalizer,
    ) -> Result<RgbaImage> {
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
        // thread performs the D3D copy, map and native-to-RGBA conversion for frame N.
        let frame = state.latest.take().expect("checked latest WGC frame above");
        state.published_at = None;
        drop(state);

        let read_start = Instant::now();
        let read_result = self.read_client_region(&frame, client_roi, converter);
        let read_elapsed = read_start.elapsed();
        let _ = frame.Close();
        let captured = read_result?;
        self.consumed_generation = generation;

        let generation_after_read = self.frame_bus.generation();
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
        client_roi: ImageRect,
        converter: &ColorNormalizer,
    ) -> Result<RgbaImage> {
        let content_size = frame.ContentSize().unwrap_or(self.item_size);
        let surface = frame.Surface().context("failed to get WGC frame surface")?;
        let texture = d3d11_texture_from_surface(&surface)?;
        let source_region = ImageRect {
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
            converter,
        )?;

        Ok(image)
    }
}

impl Drop for WgcCapture {
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
    region: ImageRect,
    cache: &mut TextureReadCache,
    converter: &ColorNormalizer,
) -> Result<RgbaImage> {
    let t0 = Instant::now();

    let mut src_desc = D3D11_TEXTURE2D_DESC::default();
    unsafe { texture.GetDesc(&mut src_desc) };

    let format = src_desc.Format;
    let (pixel_format, source_bytes_per_pixel) = match format {
        DXGI_FORMAT_B8G8R8A8_UNORM => (CapturePixelFormat::Bgra8, 4),
        DXGI_FORMAT_R16G16B16A16_FLOAT => (CapturePixelFormat::Rgba16Float, 8),
        _ => bail!("unsupported D3D11 texture format: {format:?}"),
    };

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
    #[cfg(feature = "diagnostics")]
    let mut f16_dump = (pixel_format == CapturePixelFormat::Rgba16Float
        && cache.f16_dump_dir.is_some()
        && !cache.f16_dumped)
        .then(|| Vec::with_capacity(width as usize * height as usize * 8));

    let t = Instant::now();
    for row in 0..height as usize {
        let row_ptr = unsafe { source.add(row * row_pitch) };
        let dst_row = &mut rgba[row * width as usize * 4..(row + 1) * width as usize * 4];

        let row_bytes =
            unsafe { std::slice::from_raw_parts(row_ptr, width as usize * source_bytes_per_pixel) };
        #[cfg(feature = "diagnostics")]
        if let Some(dump) = &mut f16_dump {
            dump.extend_from_slice(row_bytes);
        }
        match pixel_format {
            CapturePixelFormat::Bgra8 => converter.convert_bgra8_row(row_bytes, dst_row),
            CapturePixelFormat::Rgba16Float => converter.convert_rgba16f_row(row_bytes, dst_row),
        }
    }
    let t_convert = t.elapsed();

    unsafe { context.Unmap(&dst, 0) };

    #[cfg(feature = "diagnostics")]
    if let Some(data) = f16_dump {
        cache.f16_dumped = true;
        let directory = cache
            .f16_dump_dir
            .as_deref()
            .expect("FP16 dump data requires an output directory");
        match save_rgba16f_npy(
            directory,
            region,
            converter.diagnostic_tag(),
            width,
            height,
            &data,
        ) {
            Ok(path) => debug!(
                path = %path.display(),
                width,
                height,
                "saved raw WGC FP16 frame"
            ),
            Err(error) => warn!(error = ?error, "failed to save raw WGC FP16 frame"),
        }
    }

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
        source_format = pixel_format.label(),
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

#[cfg(feature = "diagnostics")]
fn save_rgba16f_npy(
    directory: &Path,
    region: ImageRect,
    diagnostic_tag: &str,
    width: u32,
    height: u32,
    data: &[u8],
) -> Result<PathBuf> {
    if data.len() != width as usize * height as usize * 8 {
        bail!("invalid packed RGBA16F byte count: {}", data.len());
    }

    std::fs::create_dir_all(directory)
        .with_context(|| format!("failed to create {}", directory.display()))?;
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .context("system clock is before the Unix epoch")?
        .as_millis();
    let path = directory.join(format!(
        "wgc-{timestamp}-x{}-y{}-{width}x{height}-{diagnostic_tag}.npy",
        region.x, region.y,
    ));

    let mut header =
        format!("{{'descr': '<f2', 'fortran_order': False, 'shape': ({height}, {width}, 4), }}");
    let padding = (64 - (10 + header.len() + 1) % 64) % 64;
    header.extend(std::iter::repeat_n(' ', padding));
    header.push('\n');
    let header_len = u16::try_from(header.len()).context("NPY header is too large")?;

    let file = std::fs::File::create(&path)
        .with_context(|| format!("failed to create {}", path.display()))?;
    let mut writer = BufWriter::new(file);
    writer.write_all(b"\x93NUMPY\x01\x00")?;
    writer.write_all(&header_len.to_le_bytes())?;
    writer.write_all(header.as_bytes())?;
    writer.write_all(data)?;
    writer.flush()?;
    Ok(path)
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
