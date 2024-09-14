use std::path::PathBuf;
use std::os::unix::fs::MetadataExt;
use std::{fs, io};
use std::num::NonZero;

use smithay::backend::allocator::dmabuf::Dmabuf;
use smithay::utils::{Physical, Size};
use smithay_client_toolkit::compositor::CompositorHandler;
use smithay_client_toolkit::dmabuf::{DmabufFeedback, DmabufHandler};
use smithay_client_toolkit::output::{OutputHandler, OutputState};
use smithay_client_toolkit::reexports::calloop::timer::{TimeoutAction, Timer};
use smithay_client_toolkit::reexports::client::protocol::wl_buffer::WlBuffer;
use smithay_client_toolkit::reexports::client::protocol::wl_output::{Transform, WlOutput};
use smithay_client_toolkit::reexports::client::protocol::wl_surface::WlSurface;
use smithay_client_toolkit::reexports::client::{delegate_noop, Connection, QueueHandle};
use smithay_client_toolkit::reexports::protocols::wp::linux_dmabuf::zv1::client::zwp_linux_dmabuf_feedback_v1::ZwpLinuxDmabufFeedbackV1;
use smithay_client_toolkit::registry::{ProvidesRegistryState, RegistryHandler, RegistryState};
use smithay_client_toolkit::registry_handlers;
use smithay_client_toolkit::session_lock::SessionLockHandler;
use smithay_client_toolkit::shell::wlr_layer::LayerShellHandler;
use smithay_client_toolkit::shell::xdg::window::WindowHandler;
use smithay_client_toolkit::shell::xdg::XdgSurface;
use smithay_client_toolkit::shm::{Shm, ShmHandler};

use super::{WaylandBackend, WaylandBackendEvent};

impl ProvidesRegistryState for WaylandBackend {
    fn registry(&mut self) -> &mut RegistryState {
        &mut self.registry_state
    }
    registry_handlers![OutputState,];
}
smithay_client_toolkit::delegate_registry!(WaylandBackend);

impl ShmHandler for WaylandBackend {
    fn shm_state(&mut self) -> &mut Shm {
        &mut self.shm_state
    }
}
smithay_client_toolkit::delegate_shm!(WaylandBackend);

impl OutputHandler for WaylandBackend {
    fn output_state(&mut self) -> &mut OutputState {
        &mut self.output_state
    }

    fn new_output(&mut self, conn: &Connection, qh: &QueueHandle<Self>, output: WlOutput) {}

    fn update_output(&mut self, conn: &Connection, qh: &QueueHandle<Self>, output: WlOutput) {}

    fn output_destroyed(&mut self, conn: &Connection, qh: &QueueHandle<Self>, output: WlOutput) {}
}
smithay_client_toolkit::delegate_output!(WaylandBackend);

impl CompositorHandler for WaylandBackend {
    fn scale_factor_changed(
        &mut self,
        conn: &Connection,
        qh: &QueueHandle<Self>,
        surface: &WlSurface,
        new_factor: i32,
    ) {
    }

    fn transform_changed(
        &mut self,
        conn: &Connection,
        qh: &QueueHandle<Self>,
        surface: &WlSurface,
        new_transform: Transform,
    ) {
    }

    fn frame(&mut self, conn: &Connection, qh: &QueueHandle<Self>, surface: &WlSurface, time: u32) {
    }

    fn surface_enter(
        &mut self,
        conn: &Connection,
        qh: &QueueHandle<Self>,
        surface: &WlSurface,
        output: &WlOutput,
    ) {
    }

    fn surface_leave(
        &mut self,
        conn: &Connection,
        qh: &QueueHandle<Self>,
        surface: &WlSurface,
        output: &WlOutput,
    ) {
    }
}
smithay_client_toolkit::delegate_compositor!(WaylandBackend);

impl WindowHandler for WaylandBackend {
    fn request_close(
        &mut self,
        conn: &Connection,
        qh: &QueueHandle<Self>,
        window: &smithay_client_toolkit::shell::xdg::window::Window,
    ) {
        self.events.send(WaylandBackendEvent::CloseRequest);
    }

    fn configure(
        &mut self,
        conn: &Connection,
        qh: &QueueHandle<Self>,
        window: &smithay_client_toolkit::shell::xdg::window::Window,
        configure: smithay_client_toolkit::shell::xdg::window::WindowConfigure,
        serial: u32,
    ) {
        if window != &self.graphics.window {
            error!("Received a configure request for an unknown window.");
            return;
        }
        let width = configure
            .new_size
            .0
            .map(Into::into)
            .unwrap_or(self.output_size.w);
        let height = configure
            .new_size
            .1
            .map(Into::into)
            .unwrap_or(self.output_size.h);

        let new_size = Size::<_, Physical>::from((width, height));

        if new_size != self.output_size {
            self.output_size = new_size;
        }

        // let dmabuf = Dmabuf::builder((600, 800));
    }
}

smithay_client_toolkit::delegate_xdg_shell!(WaylandBackend);
smithay_client_toolkit::delegate_xdg_window!(WaylandBackend);
