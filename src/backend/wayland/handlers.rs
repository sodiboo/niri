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
use smithay_client_toolkit::reexports::client::protocol::wl_seat::WlSeat;
use smithay_client_toolkit::reexports::client::protocol::wl_surface::WlSurface;
use smithay_client_toolkit::reexports::client::{delegate_noop, Connection, QueueHandle};
use smithay_client_toolkit::reexports::protocols::wp::linux_dmabuf::zv1::client::zwp_linux_dmabuf_feedback_v1::ZwpLinuxDmabufFeedbackV1;
use smithay_client_toolkit::registry::{ProvidesRegistryState, RegistryHandler, RegistryState};
use smithay_client_toolkit::{registry_handlers, seat};
use smithay_client_toolkit::seat::keyboard::KeyboardHandler;
use smithay_client_toolkit::seat::pointer::PointerHandler;
use smithay_client_toolkit::seat::SeatHandler;
use smithay_client_toolkit::session_lock::SessionLockHandler;
use smithay_client_toolkit::shell::wlr_layer::LayerShellHandler;
use smithay_client_toolkit::shell::xdg::window::WindowHandler;
use smithay_client_toolkit::shell::xdg::XdgSurface;
use smithay_client_toolkit::shell::WaylandSurface;
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
        self.events.send(WaylandBackendEvent::Close).unwrap();
    }

    fn configure(
        &mut self,
        conn: &Connection,
        qh: &QueueHandle<Self>,
        window: &smithay_client_toolkit::shell::xdg::window::Window,
        configure: smithay_client_toolkit::shell::xdg::window::WindowConfigure,
        serial: u32,
    ) {
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

impl KeyboardHandler for WaylandBackend {
    fn enter(
        &mut self,
        conn: &Connection,
        qh: &QueueHandle<Self>,
        keyboard: &smithay_client_toolkit::reexports::client::protocol::wl_keyboard::WlKeyboard,
        surface: &smithay_client_toolkit::reexports::client::protocol::wl_surface::WlSurface,
        serial: u32,
        raw: &[u32],
        keysyms: &[smithay::input::keyboard::Keysym],
    ) {
        info!("Keyboard::enter");
    }

    fn leave(
        &mut self,
        conn: &Connection,
        qh: &QueueHandle<Self>,
        keyboard: &smithay_client_toolkit::reexports::client::protocol::wl_keyboard::WlKeyboard,
        surface: &smithay_client_toolkit::reexports::client::protocol::wl_surface::WlSurface,
        serial: u32,
    ) {
        info!("Keyboard::leave");
    }

    fn press_key(
        &mut self,
        conn: &Connection,
        qh: &QueueHandle<Self>,
        keyboard: &smithay_client_toolkit::reexports::client::protocol::wl_keyboard::WlKeyboard,
        serial: u32,
        event: seat::keyboard::KeyEvent,
    ) {
        info!("Keyboard::press_key");
    }

    fn release_key(
        &mut self,
        conn: &Connection,
        qh: &QueueHandle<Self>,
        keyboard: &smithay_client_toolkit::reexports::client::protocol::wl_keyboard::WlKeyboard,
        serial: u32,
        event: seat::keyboard::KeyEvent,
    ) {
        info!("Keyboard::release_key");
    }

    fn update_modifiers(
        &mut self,
        conn: &Connection,
        qh: &QueueHandle<Self>,
        keyboard: &smithay_client_toolkit::reexports::client::protocol::wl_keyboard::WlKeyboard,
        serial: u32,
        modifiers: seat::keyboard::Modifiers,
        layout: u32,
    ) {
        info!("Keyboard::update_modifiers");
    }
}

smithay_client_toolkit::delegate_keyboard!(WaylandBackend);

impl PointerHandler for WaylandBackend {
    fn pointer_frame(
        &mut self,
        conn: &Connection,
        qh: &QueueHandle<Self>,
        pointer: &smithay_client_toolkit::reexports::client::protocol::wl_pointer::WlPointer,
        events: &[seat::pointer::PointerEvent],
    ) {
        info!("Pointer::pointer_frame");
    }
}

smithay_client_toolkit::delegate_pointer!(WaylandBackend);

impl SeatHandler for WaylandBackend {
    fn seat_state(&mut self) -> &mut seat::SeatState {
        &mut self.seat_state
    }

    fn new_seat(&mut self, conn: &Connection, qh: &QueueHandle<Self>, seat: WlSeat) {
        info!("Seat::new_seat");
    }

    fn new_capability(
        &mut self,
        conn: &Connection,
        qh: &QueueHandle<Self>,
        seat: WlSeat,
        capability: seat::Capability,
    ) {
        info!("Seat::new_capability: {:?}", capability);
    }

    fn remove_capability(
        &mut self,
        conn: &Connection,
        qh: &QueueHandle<Self>,
        seat: WlSeat,
        capability: seat::Capability,
    ) {
        info!("Seat::remove_capability");
    }

    fn remove_seat(&mut self, conn: &Connection, qh: &QueueHandle<Self>, seat: WlSeat) {
        info!("Seat::remove_seat");
    }
}

smithay_client_toolkit::delegate_seat!(WaylandBackend);
