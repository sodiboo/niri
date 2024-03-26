use std::cell::RefCell;
use std::cmp::{max, min};

use niri_config::{BlockOutFrom, WindowRule};
use smithay::backend::renderer::element::solid::{SolidColorBuffer, SolidColorRenderElement};
use smithay::backend::renderer::element::{AsRenderElements as _, Id, Kind};
use smithay::desktop::space::SpaceElement as _;
use smithay::desktop::{Window, WindowSurface};
use smithay::output::Output;
use smithay::reexports::wayland_protocols::xdg::decoration::zv1::server::zxdg_toplevel_decoration_v1;
use smithay::reexports::wayland_protocols::xdg::shell::server::xdg_toplevel;
use smithay::reexports::wayland_server::protocol::wl_surface::WlSurface;
use smithay::utils::{Logical, Point, Rectangle, Scale, Size, Transform};
use smithay::wayland::compositor::{send_surface_state, with_states};
use smithay::wayland::seat::WaylandFocus;
use smithay::wayland::shell::xdg::SurfaceCachedState;

use super::{ResolvedWindowRules, WindowRef};
use crate::handlers::xwayland::XUnwrap;
use crate::layout::LayoutElementRenderElement;
use crate::niri::WindowOffscreenId;
use crate::render_helpers::renderer::NiriRenderer;
use crate::render_helpers::RenderTarget;

#[derive(Debug)]
pub struct Mapped {
    pub window: Window,

    /// Up-to-date rules.
    rules: ResolvedWindowRules,

    /// Whether the window rules need to be recomputed.
    ///
    /// This is not used in all cases; for example, app ID and title changes recompute the rules
    /// immediately, rather than setting this flag.
    need_to_recompute_rules: bool,

    /// Whether this window has the keyboard focus.
    is_focused: bool,

    /// Buffer to draw instead of the window when it should be blocked out.
    block_out_buffer: RefCell<SolidColorBuffer>,
}

impl Mapped {
    pub fn new(window: Window, rules: ResolvedWindowRules) -> Self {
        Self {
            window,
            rules,
            need_to_recompute_rules: false,
            is_focused: false,
            block_out_buffer: RefCell::new(SolidColorBuffer::new((0, 0), [0., 0., 0., 1.])),
        }
    }

    /// Recomputes the resolved window rules and returns whether they changed.
    pub fn recompute_window_rules(&mut self, rules: &[WindowRule]) -> bool {
        self.need_to_recompute_rules = false;

        let new_rules = ResolvedWindowRules::compute(rules, WindowRef::Mapped(self));
        if new_rules == self.rules {
            return false;
        }

        self.rules = new_rules;
        true
    }

    pub fn recompute_window_rules_if_needed(&mut self, rules: &[WindowRule]) -> bool {
        if !self.need_to_recompute_rules {
            return false;
        }

        self.recompute_window_rules(rules)
    }

    pub fn is_focused(&self) -> bool {
        self.is_focused
    }

    pub fn set_is_focused(&mut self, is_focused: bool) {
        if self.is_focused == is_focused {
            return;
        }

        self.is_focused = is_focused;
        self.need_to_recompute_rules = true;
    }
}

impl crate::layout::LayoutElement for Mapped {
    type Id = Window;

    fn id(&self) -> &Self::Id {
        &self.window
    }

    fn size(&self) -> Size<i32, Logical> {
        self.window.geometry().size
    }

    fn buf_loc(&self) -> Point<i32, Logical> {
        Point::from((0, 0)) - self.window.geometry().loc
    }

    fn is_in_input_region(&self, point: Point<f64, Logical>) -> bool {
        let surface_local = point + self.window.geometry().loc.to_f64();
        self.window.is_in_input_region(&surface_local)
    }

    fn render<R: NiriRenderer>(
        &self,
        renderer: &mut R,
        location: Point<i32, Logical>,
        scale: Scale<f64>,
        alpha: f32,
        target: RenderTarget,
    ) -> Vec<LayoutElementRenderElement<R>> {
        let block_out = match self.rules.block_out_from {
            None => false,
            Some(BlockOutFrom::Screencast) => target == RenderTarget::Screencast,
            Some(BlockOutFrom::ScreenCapture) => target != RenderTarget::Output,
        };

        if block_out {
            let mut buffer = self.block_out_buffer.borrow_mut();
            buffer.resize(self.window.geometry().size);
            let elem = SolidColorRenderElement::from_buffer(
                &buffer,
                location.to_physical_precise_round(scale),
                scale,
                alpha,
                Kind::Unspecified,
            );
            vec![elem.into()]
        } else {
            let buf_pos = location - self.window.geometry().loc;
            self.window.render_elements(
                renderer,
                buf_pos.to_physical_precise_round(scale),
                scale,
                alpha,
            )
        }
    }

    fn request_configure(&self, rect: Rectangle<i32, Logical>) {
        match self.window.underlying_surface() {
            WindowSurface::Wayland(toplevel) => toplevel.with_pending_state(|state| {
                state.states.unset(xdg_toplevel::State::Fullscreen);
                state.size = Some(rect.size);
            }),
            WindowSurface::X11(surface) => {
                if surface.geometry().loc != rect.loc {
                    debug!("configure {:?} #{:?}", surface.title(), surface.class());
                    debug!("with new loc {:?}", rect.loc)
                }
                surface.set_fullscreen(false).xunwrap();
                surface.configure(rect).xunwrap();
            }
        }
    }

    fn request_fullscreen(&self, size: Size<i32, Logical>) {
        match self.window.underlying_surface() {
            WindowSurface::Wayland(toplevel) => toplevel.with_pending_state(|state| {
                state.size = Some(size);
                state.states.set(xdg_toplevel::State::Fullscreen);
            }),
            WindowSurface::X11(surface) => {
                surface
                    .configure(Rectangle::from_loc_and_size((0, 0), size))
                    .unwrap();
                surface.set_fullscreen(true).unwrap();
            }
        }
    }

    fn min_size(&self) -> Size<i32, Logical> {
        let mut size = with_states(&self.window.wl_surface().unwrap(), |state| {
            let curr = state.cached_state.current::<SurfaceCachedState>();
            curr.min_size
        });

        if let Some(x) = self.rules.min_width {
            size.w = max(size.w, i32::from(x));
        }
        if let Some(x) = self.rules.min_height {
            size.h = max(size.h, i32::from(x));
        }

        size
    }

    fn max_size(&self) -> Size<i32, Logical> {
        let mut size = with_states(&self.window.wl_surface().unwrap(), |state| {
            let curr = state.cached_state.current::<SurfaceCachedState>();
            curr.max_size
        });

        if let Some(x) = self.rules.max_width {
            if size.w == 0 {
                size.w = i32::from(x);
            } else if x > 0 {
                size.w = min(size.w, i32::from(x));
            }
        }
        if let Some(x) = self.rules.max_height {
            if size.h == 0 {
                size.h = i32::from(x);
            } else if x > 0 {
                size.h = min(size.h, i32::from(x));
            }
        }

        size
    }

    fn is_wl_surface(&self, wl_surface: &WlSurface) -> bool {
        self.window.wl_surface().as_ref() == Some(wl_surface)
    }

    fn is_override_redirect(&self) -> bool {
        self.window
            .x11_surface()
            .map_or(false, |surface| surface.is_override_redirect())
    }

    fn set_preferred_scale_transform(&self, scale: i32, transform: Transform) {
        self.window.with_surfaces(|surface, data| {
            send_surface_state(surface, data, scale, transform);
        });
    }

    fn has_ssd(&self) -> bool {
        match self.window.underlying_surface() {
            WindowSurface::Wayland(toplevel) => {
                toplevel.current_state().decoration_mode
                    == Some(zxdg_toplevel_decoration_v1::Mode::ServerSide)
            }
            WindowSurface::X11(surface) => !surface.is_decorated(),
        }
    }

    fn output_enter(&self, output: &Output) {
        let overlap = Rectangle::from_loc_and_size((0, 0), (i32::MAX, i32::MAX));
        self.window.output_enter(output, overlap)
    }

    fn output_leave(&self, output: &Output) {
        self.window.output_leave(output)
    }

    fn set_offscreen_element_id(&self, id: Option<Id>) {
        let data = self
            .window
            .user_data()
            .get_or_insert(WindowOffscreenId::default);
        data.0.replace(id);
    }

    fn set_activated(&mut self, active: bool) {
        match self.window.underlying_surface() {
            WindowSurface::Wayland(toplevel) => {
                let changed = toplevel.with_pending_state(|state| {
                    if active {
                        state.states.set(xdg_toplevel::State::Activated)
                    } else {
                        state.states.unset(xdg_toplevel::State::Activated)
                    }
                });
                self.need_to_recompute_rules |= changed;
            }
            WindowSurface::X11(surface) => {
                surface.set_activated(active).xunwrap();
            }
        }
    }

    fn set_bounds(&self, bounds: Size<i32, Logical>) {
        match self.window.underlying_surface() {
            WindowSurface::Wayland(toplevel) => toplevel.with_pending_state(|state| {
                state.bounds = Some(bounds);
            }),
            WindowSurface::X11(_surface) => {}
        }
    }

    fn send_pending_configure(&self) {
        match self.window.underlying_surface() {
            WindowSurface::Wayland(toplevel) => {
                toplevel.send_pending_configure();
            }
            WindowSurface::X11(_surface) => {}
        }
    }

    fn close(&self) {
        match self.window.underlying_surface() {
            WindowSurface::Wayland(toplevel) => toplevel.send_close(),
            WindowSurface::X11(surface) => surface.close().unwrap(),
        }
    }

    fn can_fullscreen(&self) -> bool {
        match self.window.underlying_surface() {
            WindowSurface::Wayland(toplevel) => toplevel
                .current_state()
                .capabilities
                .contains(xdg_toplevel::WmCapabilities::Fullscreen),
            WindowSurface::X11(_) => true,
        }
    }

    fn is_fullscreen(&self) -> bool {
        match self.window.underlying_surface() {
            WindowSurface::Wayland(toplevel) => toplevel
                .current_state()
                .states
                .contains(xdg_toplevel::State::Fullscreen),
            WindowSurface::X11(surface) => surface.is_fullscreen(),
        }
    }

    fn is_pending_fullscreen(&self) -> bool {
        match self.window.underlying_surface() {
            WindowSurface::Wayland(toplevel) => toplevel
                .with_pending_state(|state| state.states.contains(xdg_toplevel::State::Fullscreen)),
            WindowSurface::X11(surface) => surface.is_fullscreen(),
        }
    }

    fn refresh(&self) {
        self.window.refresh();
    }

    fn rules(&self) -> &ResolvedWindowRules {
        &self.rules
    }
}
