use std::rc::Rc;
use std::sync::Arc;

use smithay::backend::egl::context::{GlAttributes, PixelFormatRequirements};
use smithay::backend::egl::display::EGLDisplayHandle;
use smithay::backend::egl::native::{EGLNativeDisplay, EGLNativeSurface};
use smithay::backend::egl::{ffi, wrap_egl_call_ptr, EGLContext, EGLDisplay, EGLError, EGLSurface};
use smithay::backend::renderer::gles::GlesRenderer;
use smithay::backend::renderer::Bind;
use smithay::egl_platform;
use smithay::utils::{Physical, Rectangle, Size};
use smithay_client_toolkit::reexports::client::{Proxy, QueueHandle};
use smithay_client_toolkit::shell::xdg::window::Window;
use smithay_client_toolkit::shell::WaylandSurface;
use wayland_egl::WlEglSurface;

use super::WaylandBackend;

pub struct WaylandGraphicsBackend {
    qh: QueueHandle<WaylandBackend>,

    window: Window,
    window_size: Size<i32, Physical>,

    damage_tracking: bool,
    bind_size: Option<Size<i32, Physical>>,

    pending_frame_callback: bool,

    _display: EGLDisplay,
    surface: Rc<EGLSurface>,
    renderer: GlesRenderer,
}

impl WaylandGraphicsBackend {
    pub fn renderer(&mut self) -> &mut GlesRenderer {
        &mut self.renderer
    }

    pub fn window(&self) -> &Window {
        &self.window
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
            pending_frame_callback: false,

            _display: display,
            surface: Rc::new(surface),
            renderer,
        })
    }

    /// Request a frame callback if we don't have one for this window in flight.
    pub fn request_frame_callback(&mut self) {
        let surface = self.window.wl_surface();
        if !self.pending_frame_callback {
            surface.frame(&self.qh, surface.clone());
            self.pending_frame_callback = true;
        }
    }

    pub fn got_frame_callback(&mut self) {
        self.pending_frame_callback = false;
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
        self.request_frame_callback();
        self.surface.swap_buffers(damage.to_owned().as_deref_mut())
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
