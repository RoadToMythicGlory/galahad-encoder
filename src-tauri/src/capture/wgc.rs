//! Windows Graphics Capture backend.
//!
//! Captures a single window into BGRA frames using a free-threaded capture frame
//! pool, copies each surface to a CPU-readable staging texture, packs the rows,
//! and forwards the result. A free-threaded pool lets us poll frames from our own
//! thread without a WinRT dispatcher queue.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use crossbeam_channel::Sender;
use windows::core::{Interface, IInspectable};
use windows::Graphics::Capture::{
    Direct3D11CaptureFramePool, GraphicsCaptureItem,
};
use windows::Graphics::DirectX::DirectXPixelFormat;
use windows::Win32::Foundation::{HMODULE, HWND};
use windows::Win32::Graphics::Direct3D::{D3D_DRIVER_TYPE_HARDWARE, D3D_DRIVER_TYPE_WARP};
use windows::Win32::Graphics::Direct3D11::{
    D3D11CreateDevice, ID3D11Device, ID3D11DeviceContext, ID3D11Resource, ID3D11Texture2D,
    D3D11_CPU_ACCESS_READ, D3D11_CREATE_DEVICE_BGRA_SUPPORT, D3D11_MAP_READ,
    D3D11_MAPPED_SUBRESOURCE, D3D11_SDK_VERSION, D3D11_TEXTURE2D_DESC, D3D11_USAGE_STAGING,
};
use windows::Win32::Graphics::Dxgi::Common::DXGI_FORMAT_B8G8R8A8_UNORM;
use windows::Win32::Graphics::Dxgi::IDXGIDevice;
use windows::Win32::System::Com::{CoInitializeEx, CoUninitialize, COINIT_MULTITHREADED};
use windows::Win32::System::WinRT::Direct3D11::{
    CreateDirect3D11DeviceFromDXGIDevice, IDirect3DDxgiInterfaceAccess,
};
use windows::Win32::System::WinRT::Graphics::Capture::IGraphicsCaptureItemInterop;

use super::{CaptureConfig, CaptureSession, FrameBuffer};
use crate::error::{EncoderError, Result};
use crate::window_enum;

pub fn start(config: CaptureConfig, sink: Sender<FrameBuffer>) -> Result<CaptureSession> {
    let hwnd = window_enum::resolve(config.window_id)
        .ok_or_else(|| EncoderError::Capture("selected window no longer exists".into()))?;

    // Probe the initial size on the calling thread so we can report dimensions
    // synchronously; the capture thread re-derives them too.
    let (width, height) = window_capture_size(hwnd)?;
    if width == 0 || height == 0 {
        return Err(EncoderError::Capture(
            "selected window has zero size (is it minimized?)".into(),
        ));
    }

    let stop = Arc::new(AtomicBool::new(false));
    let thread_stop = stop.clone();
    let window_id = config.window_id;
    let fps = config.fps.max(1);

    let handle = std::thread::Builder::new()
        .name("wgc-capture".into())
        .spawn(move || {
            if let Err(e) = capture_loop(window_id, fps, sink, thread_stop) {
                log::error!("wgc capture loop ended: {e}");
            }
        })
        .map_err(|e| EncoderError::Capture(format!("failed to spawn capture thread: {e}")))?;

    Ok(CaptureSession {
        stop,
        handle: Some(handle),
        width,
        height,
    })
}

fn window_capture_size(hwnd: HWND) -> Result<(u32, u32)> {
    unsafe {
        let interop: IGraphicsCaptureItemInterop =
            windows::core::factory::<GraphicsCaptureItem, IGraphicsCaptureItemInterop>()
                .map_err(|e| EncoderError::Capture(format!("capture interop unavailable: {e}")))?;
        let item: GraphicsCaptureItem = interop
            .CreateForWindow(hwnd)
            .map_err(|e| EncoderError::Capture(format!("cannot capture window: {e}")))?;
        let size = item
            .Size()
            .map_err(|e| EncoderError::Capture(format!("cannot read window size: {e}")))?;
        Ok((size.Width.max(0) as u32, size.Height.max(0) as u32))
    }
}

fn capture_loop(
    window_id: isize,
    fps: u32,
    sink: Sender<FrameBuffer>,
    stop: Arc<AtomicBool>,
) -> Result<()> {
    unsafe {
        // MTA so the free-threaded pool callbacks and our polling co-operate.
        let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
        let result = capture_loop_inner(window_id, fps, &sink, &stop);
        CoUninitialize();
        result
    }
}

unsafe fn capture_loop_inner(
    window_id: isize,
    fps: u32,
    sink: &Sender<FrameBuffer>,
    stop: &Arc<AtomicBool>,
) -> Result<()> {
    let hwnd = window_enum::resolve(window_id)
        .ok_or_else(|| EncoderError::Capture("window vanished before capture".into()))?;

    let (device, context) = create_d3d_device()?;
    let dxgi_device: IDXGIDevice = device
        .cast()
        .map_err(|e| EncoderError::Capture(format!("dxgi device cast failed: {e}")))?;
    let inspectable: IInspectable = CreateDirect3D11DeviceFromDXGIDevice(&dxgi_device)
        .map_err(|e| EncoderError::Capture(format!("winrt device create failed: {e}")))?;
    let rt_device: windows::Graphics::DirectX::Direct3D11::IDirect3DDevice = inspectable
        .cast()
        .map_err(|e| EncoderError::Capture(format!("winrt device cast failed: {e}")))?;

    let interop: IGraphicsCaptureItemInterop =
        windows::core::factory::<GraphicsCaptureItem, IGraphicsCaptureItemInterop>()
            .map_err(|e| EncoderError::Capture(format!("capture interop unavailable: {e}")))?;
    let item: GraphicsCaptureItem = interop
        .CreateForWindow(hwnd)
        .map_err(|e| EncoderError::Capture(format!("cannot capture window: {e}")))?;

    let mut size = item
        .Size()
        .map_err(|e| EncoderError::Capture(format!("cannot read window size: {e}")))?;

    let frame_pool = Direct3D11CaptureFramePool::CreateFreeThreaded(
        &rt_device,
        DirectXPixelFormat::B8G8R8A8UIntNormalized,
        2,
        size,
    )
    .map_err(|e| EncoderError::Capture(format!("frame pool create failed: {e}")))?;

    let session = frame_pool
        .CreateCaptureSession(&item)
        .map_err(|e| EncoderError::Capture(format!("capture session create failed: {e}")))?;

    // Keep the cursor visible where the OS exposes that capture-session option.
    let _ = session.SetIsCursorCaptureEnabled(true);

    session
        .StartCapture()
        .map_err(|e| EncoderError::Capture(format!("StartCapture failed: {e}")))?;

    let frame_interval = Duration::from_secs_f64(1.0 / fps as f64);
    let mut staging: Option<(ID3D11Texture2D, u32, u32)> = None;

    while !stop.load(Ordering::SeqCst) {
        let tick = Instant::now();

        match frame_pool.TryGetNextFrame() {
            Ok(frame) => {
                let surface = frame
                    .Surface()
                    .map_err(|e| EncoderError::Capture(format!("frame surface failed: {e}")))?;
                let access: IDirect3DDxgiInterfaceAccess = surface
                    .cast()
                    .map_err(|e| EncoderError::Capture(format!("surface access failed: {e}")))?;
                let texture: ID3D11Texture2D = access
                    .GetInterface()
                    .map_err(|e| EncoderError::Capture(format!("get texture failed: {e}")))?;

                // Handle window resize: recreate the pool to the new content size.
                if let Ok(content_size) = frame.ContentSize() {
                    if content_size.Width != size.Width || content_size.Height != size.Height {
                        size = content_size;
                        let _ = frame_pool.Recreate(
                            &rt_device,
                            DirectXPixelFormat::B8G8R8A8UIntNormalized,
                            2,
                            size,
                        );
                        staging = None;
                        drop(frame);
                        continue;
                    }
                }

                let mut desc = D3D11_TEXTURE2D_DESC::default();
                texture.GetDesc(&mut desc);

                // (Re)create the staging texture when geometry changes.
                let need_new = match &staging {
                    Some((_, w, h)) => *w != desc.Width || *h != desc.Height,
                    None => true,
                };
                if need_new {
                    staging = Some((
                        create_staging_texture(&device, desc.Width, desc.Height)?,
                        desc.Width,
                        desc.Height,
                    ));
                }
                let (stage_tex, w, h) = staging.as_ref().unwrap();

                // CopyResource / Map operate on ID3D11Resource; cast explicitly.
                let dst_res: ID3D11Resource = stage_tex
                    .cast()
                    .map_err(|e| EncoderError::Capture(format!("resource cast failed: {e}")))?;
                let src_res: ID3D11Resource = texture
                    .cast()
                    .map_err(|e| EncoderError::Capture(format!("resource cast failed: {e}")))?;
                context.CopyResource(&dst_res, &src_res);

                if let Some(buffer) = map_and_pack(&context, &dst_res, *w, *h)? {
                    // Drop frames if the encoder is backed up rather than block.
                    let _ = sink.try_send(buffer);
                }
                drop(frame);
            }
            Err(_) => {
                // No frame ready yet; fall through to pacing sleep.
            }
        }

        if let Some(remaining) = frame_interval.checked_sub(tick.elapsed()) {
            std::thread::sleep(remaining);
        }
    }

    let _ = session.Close();
    let _ = frame_pool.Close();
    Ok(())
}

unsafe fn create_d3d_device() -> Result<(ID3D11Device, ID3D11DeviceContext)> {
    for driver in [D3D_DRIVER_TYPE_HARDWARE, D3D_DRIVER_TYPE_WARP] {
        let mut device: Option<ID3D11Device> = None;
        let mut context: Option<ID3D11DeviceContext> = None;
        let hr = D3D11CreateDevice(
            None,
            driver,
            HMODULE::default(),
            D3D11_CREATE_DEVICE_BGRA_SUPPORT,
            None,
            D3D11_SDK_VERSION,
            Some(&mut device),
            None,
            Some(&mut context),
        );
        if hr.is_ok() {
            if let (Some(d), Some(c)) = (device, context) {
                return Ok((d, c));
            }
        }
    }
    Err(EncoderError::Capture(
        "failed to create a Direct3D11 device".into(),
    ))
}

unsafe fn create_staging_texture(
    device: &ID3D11Device,
    width: u32,
    height: u32,
) -> Result<ID3D11Texture2D> {
    let desc = D3D11_TEXTURE2D_DESC {
        Width: width,
        Height: height,
        MipLevels: 1,
        ArraySize: 1,
        Format: DXGI_FORMAT_B8G8R8A8_UNORM,
        SampleDesc: windows::Win32::Graphics::Dxgi::Common::DXGI_SAMPLE_DESC {
            Count: 1,
            Quality: 0,
        },
        Usage: D3D11_USAGE_STAGING,
        BindFlags: 0,
        CPUAccessFlags: D3D11_CPU_ACCESS_READ.0 as u32,
        MiscFlags: 0,
    };
    let mut texture: Option<ID3D11Texture2D> = None;
    device
        .CreateTexture2D(&desc, None, Some(&mut texture))
        .map_err(|e| EncoderError::Capture(format!("staging texture failed: {e}")))?;
    texture.ok_or_else(|| EncoderError::Capture("null staging texture".into()))
}

/// Map the staging resource and copy into a tightly packed BGRA buffer.
unsafe fn map_and_pack(
    context: &ID3D11DeviceContext,
    resource: &ID3D11Resource,
    width: u32,
    height: u32,
) -> Result<Option<FrameBuffer>> {
    let mut mapped = D3D11_MAPPED_SUBRESOURCE::default();
    context
        .Map(resource, 0, D3D11_MAP_READ, 0, Some(&mut mapped))
        .map_err(|e| EncoderError::Capture(format!("map failed: {e}")))?;

    let row_bytes = (width * 4) as usize;
    let src_pitch = mapped.RowPitch as usize;
    let mut data = vec![0u8; row_bytes * height as usize];
    let src = mapped.pData as *const u8;

    for row in 0..height as usize {
        let src_row = src.add(row * src_pitch);
        let dst_start = row * row_bytes;
        std::ptr::copy_nonoverlapping(src_row, data.as_mut_ptr().add(dst_start), row_bytes);
    }

    context.Unmap(resource, 0);

    Ok(Some(FrameBuffer {
        width,
        height,
        data,
    }))
}
