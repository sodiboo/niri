use smithay::utils::{Physical, Size};
use smithay_client_toolkit::compositor::CompositorHandler;
use smithay_client_toolkit::output::{OutputHandler, OutputState};
use smithay_client_toolkit::reexports::client::protocol::wl_output::{Transform, WlOutput};
use smithay_client_toolkit::reexports::client::protocol::wl_surface::WlSurface;
use smithay_client_toolkit::reexports::client::{Connection, QueueHandle};
use smithay_client_toolkit::registry::{ProvidesRegistryState, RegistryState};
use smithay_client_toolkit::registry_handlers;
use smithay_client_toolkit::shell::xdg::window::WindowHandler;
use smithay_client_toolkit::shell::WaylandSurface;

use super::{WaylandBackend, WaylandBackendEvent};

impl ProvidesRegistryState for WaylandBackend {
    fn registry(&mut self) -> &mut RegistryState {
        &mut self.registry_state
    }
    registry_handlers![OutputState, super::seat::SeatState];
}
smithay_client_toolkit::delegate_registry!(WaylandBackend);

impl OutputHandler for WaylandBackend {
    fn output_state(&mut self) -> &mut OutputState {
        &mut self.output_state
    }

    fn new_output(&mut self, _: &Connection, _: &QueueHandle<Self>, _: WlOutput) {}
    fn update_output(&mut self, _: &Connection, _: &QueueHandle<Self>, _: WlOutput) {}
    fn output_destroyed(&mut self, _: &Connection, _: &QueueHandle<Self>, _: WlOutput) {}
}
smithay_client_toolkit::delegate_output!(WaylandBackend);

impl CompositorHandler for WaylandBackend {
    fn scale_factor_changed(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &WlSurface,
        _: i32,
    ) {
    }

    fn transform_changed(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &WlSurface,
        _: Transform,
    ) {
    }

    fn frame(&mut self, _: &Connection, _: &QueueHandle<Self>, surface: &WlSurface, _: u32) {
        assert_eq!(surface, self.graphics.window().wl_surface());
        self.send_event(WaylandBackendEvent::Frame);
        self.graphics.got_frame_callback();
    }

    fn surface_enter(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &WlSurface,
        _: &WlOutput,
    ) {
    }

    fn surface_leave(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &WlSurface,
        _: &WlOutput,
    ) {
    }
}
smithay_client_toolkit::delegate_compositor!(WaylandBackend);

impl WindowHandler for WaylandBackend {
    fn request_close(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        window: &smithay_client_toolkit::shell::xdg::window::Window,
    ) {
        assert_eq!(window, self.graphics.window());
        self.events.send(WaylandBackendEvent::Close).unwrap();
    }

    fn configure(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        window: &smithay_client_toolkit::shell::xdg::window::Window,
        configure: smithay_client_toolkit::shell::xdg::window::WindowConfigure,
        _: u32,
    ) {
        assert_eq!(window, self.graphics.window());
        let width = configure
            .new_size
            .0
            .map(u32::from)
            .map(|x| x as i32)
            .unwrap_or(self.graphics.window_size().w);
        let height = configure
            .new_size
            .1
            .map(u32::from)
            .map(|x| x as i32)
            .unwrap_or(self.graphics.window_size().h);

        let new_size = Size::<_, Physical>::from((width, height));

        if new_size != self.graphics.window_size() {
            self.graphics.set_window_size(new_size);
            self.events.send(WaylandBackendEvent::Resize).unwrap();
        }

        // let dmabuf = Dmabuf::builder((600, 800));
    }
}

smithay_client_toolkit::delegate_xdg_shell!(WaylandBackend);
smithay_client_toolkit::delegate_xdg_window!(WaylandBackend);
