use std::cell::RefCell;
use std::fmt::Debug;
use std::os::unix::io::OwnedFd;
use std::sync::Arc;

use smithay::desktop::space::SpaceElement;
use smithay::desktop::{Window, WindowSurface};
use smithay::input::pointer::Focus;
use smithay::utils::{Logical, Rectangle, SERIAL_COUNTER};
use smithay::wayland::compositor::with_states;
use smithay::wayland::selection::data_device::{
    clear_data_device_selection, current_data_device_selection_userdata,
    request_data_device_client_selection, set_data_device_selection,
};
use smithay::wayland::selection::primary_selection::{
    clear_primary_selection, current_primary_selection_userdata, request_primary_client_selection,
    set_primary_selection,
};
use smithay::wayland::selection::SelectionTarget;
use smithay::xwayland::xwm::{Reorder, ResizeEdge as X11ResizeEdge, XwmId};
use smithay::xwayland::{X11Surface, X11Wm, XwmHandler};
use tracing::{error, trace};

use crate::niri::State;
use crate::utils::clone2;
use crate::window::Unmapped;

pub trait XUnwrap {
    type T;
    fn xunwrap(self) -> Self::T;
}

impl<E: Debug> XUnwrap for Result<(), E> {
    type T = ();
    fn xunwrap(self) -> Self::T {
        if let Err(err) = self {
            error!("X11 error: {:?}", err);
        }
    }
}

impl XwmHandler for State {
    fn xwm_state(&mut self, _xwm: XwmId) -> &mut X11Wm {
        self.niri.xwm.as_mut().unwrap()
    }

    fn new_window(&mut self, _xwm: XwmId, _window: X11Surface) {}
    fn new_override_redirect_window(&mut self, _xwm: XwmId, _window: X11Surface) {}
    fn destroyed_window(&mut self, _xwm: XwmId, window: X11Surface) {}

    fn map_window_request(&mut self, _xwm: XwmId, window: X11Surface) {
        window.set_mapped(true).unwrap()
    }

    fn map_window_notify(&mut self, _xwm: XwmId, window: X11Surface) {
        debug!(
            "map {} {:?}, #{}",
            window.window_id(),
            window.title(),
            window.class()
        );
        debug!("with geometry {:?}", window.geometry());
        let wl_surface = window.wl_surface().unwrap();
        let unmapped = Unmapped::new(Window::new_x11_window(window));
        let existing = self.niri.unmapped_windows.insert(wl_surface, unmapped);

        assert!(existing.is_none());
    }

    fn mapped_override_redirect_window(&mut self, _xwm: XwmId, window: X11Surface) {
        debug!(
            "mapped_override_redirect {}, {:?}, #{}",
            window.window_id(),
            window.title(),
            window.class()
        );
        debug!("with geometry {:?}", window.geometry());
        debug!(
            "this window is transient for {:?}",
            window.is_transient_for()
        );
        // let location = window.geometry().loc;
        // let window = WindowElement(Window::new_x11_window(window));
        // self.state.space.map_element(window, location, true);
    }

    fn unmapped_window(&mut self, _xwm: XwmId, window: X11Surface) {
        debug!(
            "unmap {}, {:?}, #{}",
            window.window_id(),
            window.title(),
            window.class()
        );
        let Some(surface) = window.wl_surface() else {
            error!("unmapped_window without wl_surface");
            return;
        };
        if self.niri.unmapped_windows.remove(&surface).is_some() {
            return;
        }

        let win_out = self.niri.layout.find_window_and_output(&surface);

        let Some((window, output)) = win_out.map(clone2) else {
            // I have no idea how this can happen, but I saw it happen once, in a weird interaction
            // involving laptop going to sleep and resuming.
            error!("toplevel missing from both unmapped_windows and layout");
            return;
        };

        self.niri.layout.remove_window(&window);
        self.niri.queue_redraw(output);
    }
    fn configure_request(
        &mut self,
        _xwm: XwmId,
        window: X11Surface,
        _x: Option<i32>,
        _y: Option<i32>,
        w: Option<u32>,
        h: Option<u32>,
        _reorder: Option<Reorder>,
    ) {
        // we just set the new size, but don't let windows move themselves around freely
        let mut geo = window.geometry();
        if let Some(w) = w {
            geo.size.w = w as i32;
        }
        if let Some(h) = h {
            geo.size.h = h as i32;
        }
        let _ = window.configure(geo);
    }

    fn configure_notify(
        &mut self,
        _xwm: XwmId,
        window: X11Surface,
        geometry: Rectangle<i32, Logical>,
        _above: Option<u32>,
    ) {
        debug!(
            "configure_notify {}, {:?}, #{}",
            window.window_id(),
            window.title(),
            window.class()
        );
        // let Some(elem) = self
        //     .state
        //     .space
        //     .elements()
        //     .find(|e| matches!(e.0.x11_surface(), Some(w) if w == &window))
        //     .cloned()
        // else {
        //     return;
        // };
        // self.state.space.map_element(elem, geometry.loc, false);
        // TODO: We don't properly handle the order of override-redirect windows here,
        //       they are always mapped top and then never reordered.
    }

    fn resize_request(
        &mut self,
        _xwm: XwmId,
        window: X11Surface,
        _button: u32,
        edges: X11ResizeEdge,
    ) {
        // FIXME
    }

    fn move_request(&mut self, _xwm: XwmId, window: X11Surface, _button: u32) {
        // FIXME
    }

    fn allow_selection_access(&mut self, xwm: XwmId, _selection: SelectionTarget) -> bool {
        true
    }

    fn send_selection(
        &mut self,
        _xwm: XwmId,
        selection: SelectionTarget,
        mime_type: String,
        fd: OwnedFd,
    ) {
        match selection {
            SelectionTarget::Clipboard => {
                if let Err(err) =
                    request_data_device_client_selection(&self.niri.seat, mime_type, fd)
                {
                    error!(
                        ?err,
                        "Failed to request current wayland clipboard for Xwayland",
                    );
                }
            }
            SelectionTarget::Primary => {
                if let Err(err) = request_primary_client_selection(&self.niri.seat, mime_type, fd) {
                    error!(
                        ?err,
                        "Failed to request current wayland primary selection for Xwayland",
                    );
                }
            }
        }
    }

    fn new_selection(&mut self, _xwm: XwmId, selection: SelectionTarget, mime_types: Vec<String>) {
        trace!(?selection, ?mime_types, "Got Selection from X11",);

        match selection {
            SelectionTarget::Clipboard => set_data_device_selection(
                &self.niri.display_handle,
                &self.niri.seat,
                mime_types,
                Arc::new([]),
            ),
            SelectionTarget::Primary => set_primary_selection(
                &self.niri.display_handle,
                &self.niri.seat,
                mime_types,
                Arc::new([]),
            ),
        }
    }

    fn cleared_selection(&mut self, _xwm: XwmId, selection: SelectionTarget) {
        match selection {
            SelectionTarget::Clipboard => {
                if current_data_device_selection_userdata(&self.niri.seat).is_some() {
                    clear_data_device_selection(&self.niri.display_handle, &self.niri.seat)
                }
            }
            SelectionTarget::Primary => {
                if current_primary_selection_userdata(&self.niri.seat).is_some() {
                    clear_primary_selection(&self.niri.display_handle, &self.niri.seat)
                }
            }
        }
    }
}
