use niri_config::{Match, WindowRule};
use smithay::desktop::{Window, WindowSurface};
use smithay::wayland::compositor::with_states;
use smithay::wayland::seat::WaylandFocus;
use smithay::wayland::shell::xdg::{XdgToplevelSurfaceData, XdgToplevelSurfaceRoleAttributes};
use smithay::xwayland::X11Surface;

use crate::layout::workspace::ColumnWidth;

pub mod mapped;
pub use mapped::Mapped;

pub mod unmapped;
pub use unmapped::{InitialConfigureState, Unmapped};

/// Rules fully resolved for a window.
#[derive(Debug, PartialEq)]
pub struct ResolvedWindowRules {
    /// Default width for this window.
    ///
    /// - `None`: unset (global default should be used).
    /// - `Some(None)`: set to empty (window picks its own width).
    /// - `Some(Some(width))`: set to a particular width.
    pub default_width: Option<Option<ColumnWidth>>,

    /// Output to open this window on.
    pub open_on_output: Option<String>,

    /// Whether the window should open full-width.
    pub open_maximized: Option<bool>,

    /// Whether the window should open fullscreen.
    pub open_fullscreen: Option<bool>,

    /// Extra bound on the minimum window width.
    pub min_width: Option<u16>,
    /// Extra bound on the minimum window height.
    pub min_height: Option<u16>,
    /// Extra bound on the maximum window width.
    pub max_width: Option<u16>,
    /// Extra bound on the maximum window height.
    pub max_height: Option<u16>,

    /// Whether or not to draw the border with a solid background.
    ///
    /// `None` means using the SSD heuristic.
    pub draw_border_with_background: Option<bool>,
}

fn toplevel_window_matches(role: &XdgToplevelSurfaceRoleAttributes, m: &Match) -> bool {
    m.app_id.as_ref().map_or(true, |re| {
        role.app_id
            .as_ref()
            .map_or(false, |app_id| re.is_match(app_id))
    }) && m.title.as_ref().map_or(true, |re| {
        role.title
            .as_ref()
            .map_or(false, |title| re.is_match(title))
    })
}

fn x11_window_matches(surface: &X11Surface, m: &Match) -> bool {
    m.app_id
        .as_ref()
        .map_or(true, |re| re.is_match(&surface.class()))
        && m.title
            .as_ref()
            .map_or(true, |re| re.is_match(&surface.title()))
}

fn resolve_window_rules_for_predicate(
    rules: &[WindowRule],
    f: impl Fn(&Match) -> bool,
) -> ResolvedWindowRules {
    let _span = tracy_client::span!("resolve_window_rules_for_predicate");

    let mut resolved = ResolvedWindowRules::empty();
    let mut open_on_output = None;
    for rule in rules {
        if rule.excludes.iter().any(&f) {
            continue;
        }
        if !(rule.matches.is_empty() || rule.matches.iter().any(&f)) {
            continue;
        }

        resolved.default_width = rule
            .default_column_width
            .as_ref()
            .map(|d| d.0.map(ColumnWidth::from))
            .or(resolved.default_width);

        open_on_output = rule.open_on_output.as_deref().or(open_on_output);

        resolved.open_maximized = rule.open_maximized.or(resolved.open_maximized);
        resolved.open_fullscreen = rule.open_fullscreen.or(resolved.open_fullscreen);

        resolved.min_width = rule.min_width.or(resolved.min_width);
        resolved.min_height = rule.min_height.or(resolved.min_height);
        resolved.max_width = rule.max_width.or(resolved.max_width);
        resolved.max_height = rule.max_height.or(resolved.max_height);

        resolved.draw_border_with_background = rule
            .draw_border_with_background
            .or(resolved.draw_border_with_background);
    }

    resolved.open_on_output = open_on_output.map(ToOwned::to_owned);

    resolved
}

impl ResolvedWindowRules {
    pub const fn empty() -> Self {
        Self {
            default_width: None,
            open_on_output: None,
            open_maximized: None,
            open_fullscreen: None,
            min_width: None,
            min_height: None,
            max_width: None,
            max_height: None,
            draw_border_with_background: None,
        }
    }

    pub fn compute(rules: &[WindowRule], window: &Window) -> Self {
        let _span = tracy_client::span!("ResolvedWindowRules::compute");

        match window.underlying_surface() {
            WindowSurface::Wayland(toplevel) => with_states(toplevel.wl_surface(), |states| {
                let role = states
                    .data_map
                    .get::<XdgToplevelSurfaceData>()
                    .unwrap()
                    .lock()
                    .unwrap();

                resolve_window_rules_for_predicate(rules, |m| toplevel_window_matches(&role, m))
            }),
            WindowSurface::X11(surface) => {
                resolve_window_rules_for_predicate(rules, |m| x11_window_matches(surface, m))
            }
        };

        let mut resolved = ResolvedWindowRules::empty();

        with_states(&window.wl_surface().unwrap(), |states| {
            let role = states
                .data_map
                .get::<XdgToplevelSurfaceData>()
                .unwrap()
                .lock()
                .unwrap();

            let mut open_on_output = None;

            for rule in rules {
                if !(rule.matches.is_empty()
                    || rule.matches.iter().any(|m| window_matches(&role, m)))
                {
                    continue;
                }

                if rule.excludes.iter().any(|m| window_matches(&role, m)) {
                    continue;
                }
            }

            resolved.open_on_output = open_on_output.map(|x| x.to_owned());
        });

        resolved
    }
}

fn window_matches(role: &XdgToplevelSurfaceRoleAttributes, m: &Match) -> bool {
    if let Some(app_id_re) = &m.app_id {
        let Some(app_id) = &role.app_id else {
            return false;
        };
        if !app_id_re.is_match(app_id) {
            return false;
        }
    }

    if let Some(title_re) = &m.title {
        let Some(title) = &role.title else {
            return false;
        };
        if !title_re.is_match(title) {
            return false;
        }
    }

    true
}
