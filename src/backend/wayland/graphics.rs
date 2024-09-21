use std::cell::RefCell;
use std::collections::HashMap;
use std::num::NonZeroU32;
use std::os::fd::{AsFd, OwnedFd};
use std::os::unix::fs::MetadataExt;
use std::path::PathBuf;
use std::ptr::NonNull;
use std::rc::Rc;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use std::{fs, io, mem, time};

use calloop::channel::Sender;
use niri_config::{Config, OutputName};
use smithay::backend::allocator::gbm::GbmDevice;
use smithay::backend::allocator::{Fourcc, Modifier};
use smithay::backend::drm::DrmNode;
use smithay::backend::egl::context::{GlAttributes, PixelFormatRequirements};
use smithay::backend::egl::display::EGLDisplayHandle;
use smithay::backend::egl::native::{EGLNativeDisplay, EGLNativeSurface};
use smithay::backend::egl::{ffi, wrap_egl_call_bool, wrap_egl_call_ptr, EGLContext, EGLDevice, EGLDisplay, EGLError, EGLSurface};
use smithay::backend::renderer::damage::OutputDamageTracker;
use smithay::backend::renderer::gles::GlesRenderer;
use smithay::backend::renderer::{Bind, DebugFlags, ImportDma, ImportEgl, Renderer};
use smithay::backend::winit::{self, WinitEvent, WinitEventLoop, WinitGraphicsBackend};
use smithay::egl_platform;
use smithay::output::{Mode, Output, PhysicalProperties, Subpixel};
use smithay::reexports::rustix::fs::OFlags;
use smithay::utils::{Physical, Rectangle, Size};
use smithay_client_toolkit::compositor::{CompositorState, Surface};
use smithay_client_toolkit::dmabuf::{DmabufFeedback, DmabufHandler, DmabufState};
use smithay_client_toolkit::output::OutputState;
use smithay_client_toolkit::reexports::calloop_wayland_source::WaylandSource;
use smithay_client_toolkit::reexports::client::globals::registry_queue_init;
use smithay_client_toolkit::reexports::client::protocol::wl_buffer::{self, WlBuffer};
use smithay_client_toolkit::reexports::client::protocol::wl_display::WlDisplay;
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

pub struct WaylandGraphicsBackend {
    qh: QueueHandle<WaylandBackend>,

    window: Window,
    window_size: Size<i32, Physical>,

    damage_tracking: bool,
    bind_size: Option<Size<i32, Physical>>,
    frame_callback_state: FrameCallbackState,

    display: EGLDisplay,
    surface: Rc<EGLSurface>,
    renderer: GlesRenderer,
}

/// The state of the frame callback.
#[derive(Default, Debug, Clone, Copy, PartialEq, Eq)]
pub enum FrameCallbackState {
    /// No frame callback was requested.
    #[default]
    None,
    /// The frame callback was requested, but not yet arrived, the redraw events are throttled.
    Requested,
    /// The callback was marked as done, and user could receive redraw requested
    Received,
}

impl WaylandGraphicsBackend {
    pub fn renderer(&mut self) -> &mut GlesRenderer {
        &mut self.renderer
    }

    pub fn window_size(&self) -> Size<i32, Physical> {
        self.window_size
    }

    pub fn set_window_size(&mut self, size: Size<i32, Physical>) {
        self.window_size = size;
    }

    pub fn new(
        window: Window,
        initial_size: Size<i32, Physical>,
        qh: &QueueHandle<WaylandBackend>,
    ) -> anyhow::Result<Self> {
        let backend = window
            .wl_surface()
            .backend()
            .upgrade()
            .expect("Wayland connection closed");
        let display = unsafe { EGLDisplay::new(WaylandBackendNativeDisplay(backend)) }?;

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
        let surface = WlEglSurface::new(window.wl_surface().id(), 1, 1)?;

        let surface = unsafe {
            EGLSurface::new(
                &display,
                context.pixel_format().unwrap(),
                context.config_id(),
                WaylandBackendNativeSurface(surface),
            )
        }?;

        let renderer = unsafe { GlesRenderer::new(context) }?;

        Ok(Self {
            qh: qh.clone(),

            window,
            window_size: initial_size,

            damage_tracking: display.supports_damage(),
            bind_size: None,
            frame_callback_state: FrameCallbackState::None,

            display,
            surface: Rc::new(surface),
            renderer,
        })
    }

    /// Request a frame callback if we don't have one for this window in flight.
    pub fn request_frame_callback(&mut self) {
        let surface = self.window.wl_surface();
        match self.frame_callback_state {
            FrameCallbackState::None | FrameCallbackState::Received => {
                self.frame_callback_state = FrameCallbackState::Requested;
                surface.frame(&self.qh, surface.clone());
            }
            FrameCallbackState::Requested => (),
        }
    }

    // #[instrument(level = "trace", parent = &self.span, skip(self))]
    // #[profiling::function]
    pub fn bind(&mut self) -> Result<(), smithay::backend::SwapBuffersError> {
        // NOTE: we must resize before making the current context current, otherwise the back
        // buffer will be latched. Some nvidia drivers may not like it, but a lot of wayland
        // software does the order that way due to mesa latching back buffer on each
        // `make_current`.
        if Some(self.window_size) != self.bind_size {
            self.surface
                .resize(self.window_size.w, self.window_size.h, 0, 0);
        }
        self.bind_size = Some(self.window_size);

        self.renderer.bind(self.surface.clone())?;

        Ok(())
    }

    // #[instrument(level = "trace", parent = &self.span, skip(self))]
    pub fn buffer_age(&self) -> Option<usize> {
        if self.damage_tracking {
            self.surface.buffer_age().map(|x| x as usize)
        } else {
            Some(0)
        }
    }

    // #[instrument(level = "trace", parent = &self.span, skip(self))]
    // #[profiling::function]
    pub fn submit(
        &mut self,
        damage: Option<&[Rectangle<i32, Physical>]>,
    ) -> Result<(), smithay::backend::SwapBuffersError> {
        let mut damage = match damage {
            Some(damage) if self.damage_tracking && !damage.is_empty() => {
                let bind_size = self
                    .bind_size
                    .expect("submitting without ever binding the renderer.");
                let damage = damage
                    .iter()
                    .map(|rect| {
                        Rectangle::from_loc_and_size(
                            (rect.loc.x, bind_size.h - rect.loc.y - rect.size.h),
                            rect.size,
                        )
                    })
                    .collect::<Vec<_>>();
                Some(damage)
            }
            _ => None,
        };

        // Request frame callback.
        self.request_frame_callback();
        self.surface.swap_buffers(damage.as_deref_mut())?;
        Ok(())
    }
}

struct WaylandBackendNativeDisplay(wayland_backend::client::Backend);

impl EGLNativeDisplay for WaylandBackendNativeDisplay {
    fn supported_platforms(&self) -> Vec<smithay::backend::egl::native::EGLPlatform<'_>> {
        let display = self.0.display_ptr();

        use smithay::backend::egl::native::EGLPlatform; // macro internals use it
        vec![
            // see: https://www.khronos.org/registry/EGL/extensions/KHR/EGL_KHR_platform_wayland.txt
            egl_platform!(PLATFORM_WAYLAND_KHR, display, &["EGL_KHR_platform_wayland"]),
            // see: https://www.khronos.org/registry/EGL/extensions/EXT/EGL_EXT_platform_wayland.txt
            egl_platform!(PLATFORM_WAYLAND_EXT, display, &["EGL_EXT_platform_wayland"]),
            // see: https://raw.githubusercontent.com/google/angle/main/extensions/EGL_ANGLE_platform_angle.txt
            egl_platform!(
                PLATFORM_ANGLE_ANGLE,
                display,
                &[
                    "EGL_ANGLE_platform_angle",
                    "EGL_ANGLE_platform_angle_vulkan",
                    "EGL_EXT_platform_wayland",
                ],
                vec![
                    ffi::egl::PLATFORM_ANGLE_NATIVE_PLATFORM_TYPE_ANGLE,
                    ffi::egl::PLATFORM_WAYLAND_EXT as _,
                    ffi::egl::PLATFORM_ANGLE_TYPE_ANGLE,
                    ffi::egl::PLATFORM_ANGLE_TYPE_VULKAN_ANGLE,
                    ffi::egl::NONE as ffi::EGLint
                ]
            ),
        ]
    }
}

// WlEglSurface impl is used by the winit backend
// We don't want to depend on winit backend indirectly.
struct WaylandBackendNativeSurface(WlEglSurface);

unsafe impl EGLNativeSurface for WaylandBackendNativeSurface {
    unsafe fn create(
        &self,
        display: &Arc<EGLDisplayHandle>,
        config_id: ffi::egl::types::EGLConfig,
    ) -> Result<*const std::ffi::c_void, EGLError> {
        const SURFACE_ATTRIBUTES: [std::ffi::c_int; 3] = [
            ffi::egl::RENDER_BUFFER as std::ffi::c_int,
            ffi::egl::BACK_BUFFER as std::ffi::c_int,
            ffi::egl::NONE as std::ffi::c_int,
        ];

        wrap_egl_call_ptr(|| unsafe {
            ffi::egl::CreatePlatformWindowSurfaceEXT(
                display.handle,
                config_id,
                self.0.ptr() as *mut _,
                SURFACE_ATTRIBUTES.as_ptr(),
            )
        })
    }

    fn resize(&self, width: i32, height: i32, dx: i32, dy: i32) -> bool {
        WlEglSurface::resize(&self.0, width, height, dx, dy);
        true
    }

    fn identifier(&self) -> Option<String> {
        Some("niri/nested".into())
    }
}
