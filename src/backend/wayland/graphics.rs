use smithay::backend::egl::EGLDisplay;
use smithay_client_toolkit::compositor::{CompositorHandler, SurfaceData, SurfaceDataExt};
use smithay_client_toolkit::output::{OutputHandler, OutputState};
use smithay_client_toolkit::reexports::calloop::timer::{TimeoutAction, Timer};
use smithay_client_toolkit::reexports::client::protocol::wl_output::{Transform, WlOutput};
use smithay_client_toolkit::reexports::client::protocol::wl_surface::{self, WlSurface};
use smithay_client_toolkit::reexports::client::{Connection, Dispatch, QueueHandle};
use smithay_client_toolkit::registry::{ProvidesRegistryState, RegistryHandler, RegistryState};
use smithay_client_toolkit::registry_handlers;
use smithay_client_toolkit::session_lock::SessionLockHandler;
use smithay_client_toolkit::shell::wlr_layer::LayerShellHandler;
use smithay_client_toolkit::shell::xdg::window::Window;
use smithay_client_toolkit::shm::{Shm, ShmHandler};

use super::WaylandBackend;

pub struct WaylandGraphicsBackend {
    _display: EGLDisplay,
}

impl WaylandGraphicsBackend {}
