use std::fmt::Debug;
use std::os::unix::io::OwnedFd;
use std::sync::Arc;

use smithay::desktop::Window;
use smithay::utils::{Logical, Rectangle};
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

use crate::layout::workspace::WorkspaceId;
use crate::layout::LayoutElement;
use crate::niri::State;
use crate::window::Unmapped;

pub trait XUnwrap {
    type T;
    fn xunwrap(self) -> Self::T;
}

impl<E: Debug> XUnwrap for Result<(), E> {
    type T = ();
    fn xunwrap(self) -> Self::T {
        if let Err(err) = self {
            error!("we are ignoring an X11 error: {:?}", err);
        }
    }
}

// HORRIBLE HACK: X11 windows need an absolute position, and we can't just make shit up
// This is because they use that position to place "override redirect" windows.
// Override redirect windows are not directly associated to any parent,
// and while we "could" maybe correlate them via window class, client, etc
// that approach is quite fragile and therefore undesirable
//
// It is also not advisable to give them real screenspace coordinates,
// because in niri this will overlap on output edges
//
// My solution? Make them store your metadata:
// - The window position is 32 bits
// - Reserve MSB=0; that one is a sign bit.
// - We have 31 bits left for x and y; 62 in total
// - Reserve the top 16 (of what's left) in the y coordinate
// - That leaves 15 bits: 32768 y positions should be enough
// - For the x position, that's not enough, as niri scrolls far horizontally:
// - We give the x coordinate 8 more bits; reserve only 8
// That leaves us with 24 reserved bits.
// We're not touching the width or height, because that's gonna fuck with the layout.
//
// The memory layout of the x and y coordinates, as well as the workspace id looks like this:
// y coordimate: 0YYYYYYYYYYYYYYYY...............
// x coordinate: 0XXXXXXXX.......................
// workspace id: 00000000YYYYYYYYYYYYYYYYXXXXXXXX
// where "Y" and "X" are reserved parts of Y and X coords; and . is the actual "position" of this
// window.
//
// This way, we can associate an override redirect window with the correct workspace; and the
// correct position relative to that workspace. This is necessary, because in niri, window "popups"
// should not overlap the next screen over if they are out of bounds. If we reported real
// screenspace coordinates, we could not disambiguate out-of-bounds on one screen from the next
// screen over.
//
// A workspace id of 0 is reserved for window positions representing "screenspace" windows; i.e
// notification toasts that appear in the corner of your monitor. Since clients may ignore their own
// position and spawn windows like this, we must be able to handle them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RichGeometry {
    Absolute(Rectangle<i32, Logical>),
    Workspace(WorkspaceId, Rectangle<i32, Logical>),
}

impl RichGeometry {
    pub fn into_fake(self) -> Rectangle<i32, Logical> {
        match self {
            Self::Absolute(rect) => rect,
            Self::Workspace(workspace_id, rect) => {
                let workspace_id = workspace_id.inner_u32() as i32;
                let x = rect.loc.x | ((workspace_id & 0xFF) << 23);
                let y = rect.loc.y | ((workspace_id >> 8) << 15);
                Rectangle::from_loc_and_size((x, y), rect.size)
            }
        }
    }

    pub fn from_fake(rect: Rectangle<i32, Logical>) -> Self {
        let real_x = rect.loc.x & 0x7FFFFF;
        let real_y = rect.loc.y & 0x007FFF;
        let workspace_id = ((rect.loc.y >> 15) << 8) | (rect.loc.x >> 23);
        let rect = Rectangle::from_loc_and_size((real_x, real_y), rect.size);
        if workspace_id == 0 {
            Self::Absolute(rect)
        } else {
            Self::Workspace(WorkspaceId::new_from_u32(workspace_id as u32), rect)
        }
    }
}

impl XwmHandler for State {
    fn xwm_state(&mut self, _xwm: XwmId) -> &mut X11Wm {
        self.niri.xwm.as_mut().unwrap()
    }

    fn new_window(&mut self, _xwm: XwmId, _window: X11Surface) {}
    fn new_override_redirect_window(&mut self, _xwm: XwmId, _window: X11Surface) {}
    fn destroyed_window(&mut self, _xwm: XwmId, _window: X11Surface) {}

    fn map_window_request(&mut self, _xwm: XwmId, window: X11Surface) {
        window.set_mapped(true).unwrap()
    }

    fn map_window_notify(&mut self, _xwm: XwmId, window: X11Surface) {
        if window.is_override_redirect() {
            debug!(
                "override redirect mapped: fake: {:?}, real: {:?}",
                window.geometry().loc,
                RichGeometry::from_fake(window.geometry())
            );
            self.niri.override_redirect.push(window.clone());
            self.niri.queue_redraw_all();
            return;
        }
        let wl_surface = window.wl_surface().unwrap();
        let window = Window::new_x11_window(window);
        let unmapped = Unmapped::new(window.clone());
        let existing = self.niri.unmapped_windows.insert(wl_surface, unmapped);
        assert!(existing.is_none());
        self.send_initial_configure(&window);
    }

    fn mapped_override_redirect_window(&mut self, _xwm: XwmId, _window: X11Surface) {}

    fn unmapped_window(&mut self, _xwm: XwmId, window: X11Surface) {
        if window.is_override_redirect() {
            if let Some(index) = self
                .niri
                .override_redirect
                .iter()
                .position(|w| w == &window)
            {
                self.niri.override_redirect.remove(index);
                self.niri.queue_redraw_all();
            }
            return;
        }
        let Some(surface) = window.wl_surface() else {
            error!("unmapped_window without wl_surface");
            return;
        };
        if self.niri.unmapped_windows.remove(&surface).is_some() {
            return;
        }

        let win_out = self.niri.layout.find_window_and_output(&surface);

        let Some((window, output)) = win_out else {
            error!("X11Surface missing from both unmapped_windows and layout");
            return;
        };

        let (window, output) = &(window.id().clone(), output.clone());

        self.niri.layout.remove_window(window);
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

        debug!(
            "override redirect reconfigure: fake: {:?}, real: {:?}",
            window.geometry().loc,
            RichGeometry::from_fake(window.geometry())
        );
        let _ = window.configure(geo);
    }

    fn configure_notify(
        &mut self,
        _xwm: XwmId,
        _window: X11Surface,
        _geometry: Rectangle<i32, Logical>,
        _above: Option<u32>,
    ) {
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
        _window: X11Surface,
        _button: u32,
        _edges: X11ResizeEdge,
    ) {
        // FIXME
    }

    fn move_request(&mut self, _xwm: XwmId, _window: X11Surface, _button: u32) {
        // FIXME
    }

    fn allow_selection_access(&mut self, _xwm: XwmId, _selection: SelectionTarget) -> bool {
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
