use std::cell::RefCell;
use std::collections::HashMap;
use std::num::NonZeroU32;
use std::os::fd::{AsFd, OwnedFd};
use std::os::unix::fs::MetadataExt;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use std::{fs, io, mem, time};

use calloop::channel::Sender;
use niri_config::{Config, OutputName};
use smithay::backend::allocator::gbm::GbmDevice;
use smithay::backend::allocator::{Fourcc, Modifier};
use smithay::backend::drm::DrmNode;
use smithay::backend::egl::context::PixelFormatRequirements;
use smithay::backend::egl::native::EGLNativeSurface;
use smithay::backend::egl::{EGLDevice, EGLDisplay};
use smithay::backend::renderer::damage::OutputDamageTracker;
use smithay::backend::renderer::gles::GlesRenderer;
use smithay::backend::renderer::{DebugFlags, ImportDma, ImportEgl, Renderer};
use smithay::backend::winit::{self, WinitEvent, WinitEventLoop, WinitGraphicsBackend};
use smithay::output::{Mode, Output, PhysicalProperties, Subpixel};
use smithay::reexports::rustix::fs::OFlags;
use smithay::utils::{Physical, Size};
use smithay_client_toolkit::compositor::{CompositorState, Surface};
use smithay_client_toolkit::dmabuf::{DmabufFeedback, DmabufHandler, DmabufState};
use smithay_client_toolkit::output::OutputState;
use smithay_client_toolkit::reexports::calloop_wayland_source::WaylandSource;
use smithay_client_toolkit::reexports::client::globals::registry_queue_init;
use smithay_client_toolkit::reexports::client::protocol::wl_buffer::{self, WlBuffer};
use smithay_client_toolkit::reexports::client::protocol::wl_shm;
use smithay_client_toolkit::reexports::client::protocol::wl_surface::WlSurface;
use smithay_client_toolkit::reexports::client::{delegate_dispatch, delegate_noop, Connection, Dispatch, EventQueue, Proxy, QueueHandle};
use smithay_client_toolkit::reexports::csd_frame::WindowState;
use smithay_client_toolkit::reexports::protocols::wp::linux_dmabuf::zv1::client::zwp_linux_buffer_params_v1;
use smithay_client_toolkit::reexports::protocols::wp::linux_dmabuf::zv1::client::zwp_linux_dmabuf_feedback_v1::ZwpLinuxDmabufFeedbackV1;
use smithay_client_toolkit::reexports::protocols::wp::linux_dmabuf::zv1::client::zwp_linux_dmabuf_v1::ZwpLinuxDmabufV1;
use smithay_client_toolkit::registry::RegistryState;
use smithay_client_toolkit::shell::xdg::window::{Window, WindowDecorations};
use smithay_client_toolkit::shell::xdg::XdgShell;
use smithay_client_toolkit::shell::WaylandSurface;
use smithay_client_toolkit::shm::raw::RawPool;
use smithay_client_toolkit::shm::{Shm, ShmHandler};
use wayland_egl::WlEglSurface;

use super::WaylandBackend;

#[derive(Debug)]
pub struct Plane {
    pub fd: OwnedFd,
    pub plane_idx: u32,
    pub offset: u32,
    pub stride: u32,
}

#[derive(Debug)]
pub struct Dmabuf {
    pub display: EGLDisplay,
    pub width: i32,
    pub height: i32,
    pub planes: Vec<Plane>,
    pub format: Fourcc,
    pub modifier: u64,
}

#[derive(Debug)]
pub struct Shmbuf {
    pub pool: RawPool,
    pub offset: i32,
    pub width: i32,
    pub height: i32,
    pub stride: i32,
    pub format: wl_shm::Format,
}

#[derive(Debug)]
pub enum BufferSource {
    Shm(Shmbuf),
    Dma(Dmabuf),
}

impl From<Shmbuf> for BufferSource {
    fn from(buf: Shmbuf) -> Self {
        Self::Shm(buf)
    }
}

impl From<Dmabuf> for BufferSource {
    fn from(buf: Dmabuf) -> Self {
        Self::Dma(buf)
    }
}

pub struct WaylandGraphicsBackend {
    pub backing: Arc<BufferSource>,
    pub buffer: WlBuffer,
    pub size: (u32, u32),
}

impl WaylandBackend {
    fn create_shm_buffer(
        &self,
        format: wl_shm::Format,
        width: u32,
        height: u32,
        stride: u32,
    ) -> anyhow::Result<WaylandGraphicsBackend> {
        let mut pool = RawPool::new((stride * height) as usize, &self.shm_state)?;

        let buffer = pool.create_buffer(
            0,
            width as i32,
            height as i32,
            stride as i32,
            format,
            (),
            &self.qh,
        );

        Ok(WaylandGraphicsBackend {
            backing: Arc::new(
                Shmbuf {
                    pool,
                    offset: 0,
                    width: width as i32,
                    height: height as i32,
                    stride: stride as i32,
                    format,
                }
                .into(),
            ),
            buffer,
            size: (width, height),
        })
    }

    pub fn create_gbm_buffer(
        &self,
        gbm: GbmDevice<fs::File>,
        feedback: DmabufFeedback,
        format: Fourcc,
        // modifiers: &[u64],
        (width, height): (u32, u32),
        needs_linear: bool,
    ) -> anyhow::Result<Option<WaylandGraphicsBackend>> {
        let formats = feedback.format_table();

        let modifiers = feedback
            .tranches()
            .iter()
            .flat_map(|x| &x.formats)
            .filter_map(|x| formats.get(*x as usize))
            .filter(|x| {
                x.format == format as u32
                    && (!needs_linear || x.modifier == u64::from(Modifier::Linear))
            })
            .map(|x| Modifier::from(x.modifier))
            // .filter(|x| modifiers.contains(&u64::from(*x)))
            .collect::<Vec<_>>();

        if modifiers.is_empty() {
            return Ok(None);
        };
        //dbg!(format, modifiers);
        let bo = if !modifiers.iter().all(|x| *x == Modifier::Invalid) {
            gbm.create_buffer_object_with_modifiers::<()>(
                width,
                height,
                format,
                modifiers.iter().copied(),
            )?
        } else {
            gbm.create_buffer_object::<()>(
                width,
                height,
                format,
                smithay::backend::allocator::gbm::GbmBufferFlags::empty(),
            )?
        };

        let mut planes = Vec::new();

        let params = self.dmabuf_state.create_params(&self.qh)?;
        let modifier = bo.modifier()?;
        for i in 0..bo.plane_count()? as i32 {
            let plane_fd = bo.fd_for_plane(i)?;
            let plane_offset = bo.offset(i)?;
            let plane_stride = bo.stride_for_plane(i)?;
            params.add(
                plane_fd.as_fd(),
                i as u32,
                plane_offset,
                plane_stride,
                modifier.into(),
            );
            planes.push(Plane {
                fd: plane_fd,
                plane_idx: i as u32,
                offset: plane_offset,
                stride: plane_stride,
            });
        }

        let buffer = params
            .create_immed(
                width as i32,
                height as i32,
                format as u32,
                zwp_linux_buffer_params_v1::Flags::empty(),
                &self.qh,
            )
            .0;

        let display = unsafe { EGLDisplay::new(gbm) }?;

        let gl_attributes = GlAttributes {
            version: (3, 0),
            profile: None,
            debug: cfg!(debug_assertions),
            vsync: false,
        };

        let context = EGLContext::new_with_config(
            &display,
            gl_attributes,
            PixelFormatRequirements::_10_bit(),
        )
        .or_else(|_| {
            EGLContext::new_with_config(&display, gl_attributes, PixelFormatRequirements::_8_bit())
        })?;

        let surface = WlEglSurface::new(
            self.main_window.wl_surface().id(),
            width as i32,
            height as i32,
        )?;

        Ok(Some(WaylandGraphicsBackend {
            backing: Arc::new(
                Dmabuf {
                    display,
                    width: width as i32,
                    height: height as i32,
                    planes,
                    format,
                    modifier: modifier.into(),
                }
                .into(),
            ),
            buffer,
            size: (width, height),
        }))
    }
}

fn find_gbm_device(dev: u64) -> io::Result<Option<(PathBuf, GbmDevice<fs::File>)>> {
    for i in fs::read_dir("/dev/dri")? {
        let i = i?;
        if i.metadata()?.rdev() == dev {
            let file = fs::File::options().read(true).write(true).open(i.path())?;
            log::info!("Opened gbm main device '{}'", i.path().display());
            return Ok(Some((i.path(), GbmDevice::new(file)?)));
        }
    }
    Ok(None)
}

delegate_noop!(WaylandBackend: ignore WlBuffer);

impl DmabufHandler for WaylandBackend {
    fn dmabuf_state(&mut self) -> &mut smithay_client_toolkit::dmabuf::DmabufState {
        &mut self.dmabuf_state
    }

    fn dmabuf_feedback(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _proxy: &ZwpLinuxDmabufFeedbackV1,
        feedback: DmabufFeedback,
    ) {
        #[allow(clippy::unnecessary_cast)]
        match find_gbm_device(feedback.main_device() as u64) {
            Ok(Some((path, device))) => {
                self.got_gbm_device(path, device, feedback);
                return;
            }
            Ok(None) => {
                error!("Gbm main device '{}' not found", feedback.main_device());
            }
            Err(err) => {
                error!("Failed to open gbm main device: {}", err);
            }
        }

        self.failed_gbm_device(feedback);
    }

    fn created(
        &mut self,
        conn: &Connection,
        qh: &QueueHandle<Self>,
        params: &smithay::reexports::wayland_protocols::wp::linux_dmabuf::zv1::client::zwp_linux_buffer_params_v1::ZwpLinuxBufferParamsV1,
        buffer: smithay_client_toolkit::reexports::client::protocol::wl_buffer::WlBuffer,
    ) {
        info!("Dmabuf created: {:?}", buffer);
    }

    fn failed(
        &mut self,
        conn: &Connection,
        qh: &QueueHandle<Self>,
        params: &smithay::reexports::wayland_protocols::wp::linux_dmabuf::zv1::client::zwp_linux_buffer_params_v1::ZwpLinuxBufferParamsV1,
    ) {
        error!("Dmabuf failed: {:?}", params);
    }

    fn released(
        &mut self,
        conn: &Connection,
        qh: &QueueHandle<Self>,
        buffer: &smithay_client_toolkit::reexports::client::protocol::wl_buffer::WlBuffer,
    ) {
        info!("Dmabuf released: {:?}", buffer);
    }
}

smithay_client_toolkit::delegate_dmabuf!(WaylandBackend);

// WlEglSurface impl is used by the winit backend
// We don't want to depend on winit backend indirectly.
struct WaylandBackendNativeSurface(WlEglSurface);

unsafe impl EGLNativeSurface for WaylandBackendNativeSurface {
    unsafe fn create(
        &self,
        display: &Arc<EGLDisplayHandle>,
        config_id: smithay::backend::ffi::egl::types::EGLConfig,
    ) -> Result<*const c_void, super::EGLError> {
        smithay::backend::egl::wrap_egl_call_ptr(|| unsafe {
            smithay::backend::egl::ffi::egl::CreatePlatformWindowSurfaceEXT(
                display.handle,
                config_id,
                self.ptr() as *mut _,
                WINIT_SURFACE_ATTRIBUTES.as_ptr(),
            )
        })
    }

    fn resize(&self, width: i32, height: i32, dx: i32, dy: i32) -> bool {
        wegl::WlEglSurface::resize(self, width, height, dx, dy);
        true
    }

    fn identifier(&self) -> Option<String> {
        Some("Winit/Wayland".into())
    }
}
