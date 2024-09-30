use std::cell::RefCell;
use std::collections::HashMap;
use std::mem;
use std::rc::Rc;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use calloop::channel::Sender;
use niri_config::{Action, Config, OutputName};
use smithay::backend::allocator::dmabuf::Dmabuf;
use smithay::backend::input::InputEvent;
use smithay::backend::renderer::damage::OutputDamageTracker;
use smithay::backend::renderer::gles::GlesRenderer;
use smithay::backend::renderer::{DebugFlags, ImportDma, ImportEgl, Renderer};
use smithay::output::{Mode, Output, PhysicalProperties, Subpixel};
use smithay::reexports::calloop::LoopHandle;
use smithay::reexports::wayland_protocols::wp::presentation_time::server::wp_presentation_feedback;
use smithay::utils::{Logical, Point, Transform};
use smithay_client_toolkit::compositor::{CompositorState, Surface};
use smithay_client_toolkit::output::OutputState;
use smithay_client_toolkit::reexports::calloop_wayland_source::WaylandSource;
use smithay_client_toolkit::reexports::client::globals::registry_queue_init;
use smithay_client_toolkit::reexports::client::protocol::wl_output;
use smithay_client_toolkit::reexports::client::Connection;
use smithay_client_toolkit::reexports::protocols::wp::pointer_constraints::zv1::client::zwp_locked_pointer_v1::ZwpLockedPointerV1;
use smithay_client_toolkit::registry::RegistryState;
use smithay_client_toolkit::seat::pointer_constraints::PointerConstraintsState;
use smithay_client_toolkit::seat::relative_pointer::RelativePointerState;
use smithay_client_toolkit::shell::xdg::window::WindowDecorations;
use smithay_client_toolkit::shell::xdg::XdgShell;
use smithay_client_toolkit::shell::WaylandSurface;

use super::{IpcOutputMap, OutputId, RenderResult};
use crate::niri::{Niri, RedrawState, State};
use crate::render_helpers::debug::draw_damage;
use crate::render_helpers::{resources, shaders, RenderTarget};
use crate::utils::{get_monotonic_time, logical_output};

mod graphics;
mod handlers;
mod input;
pub mod seat;

use graphics::WaylandGraphicsBackend;
pub use input::{RawAbsolutePosition, WaylandInputBackend, WaylandInputSpecialEvent};
use seat::SeatState;

#[allow(dead_code)]
pub struct WaylandBackend {
    config: Rc<RefCell<Config>>,
    events: Sender<WaylandBackendEvent>,

    registry_state: RegistryState,
    seat_state: SeatState,
    relative_pointer_state: RelativePointerState,
    output_state: OutputState,
    compositor_state: CompositorState,
    xdg_state: XdgShell,
    pointer_constraints_state: PointerConstraintsState,

    // This field is redundant with the seat state, but it's used to avoid mutex overhead on every
    // refresh.
    locked_pointers: Vec<ZwpLockedPointerV1>,

    // Also used to avoid running logic on every refresh if the cursor hasn't moved.
    prev_cursor_location: Point<f64, Logical>,

    output: Output,
    damage_tracker: OutputDamageTracker,
    ipc_outputs: Arc<Mutex<IpcOutputMap>>,

    graphics: WaylandGraphicsBackend,
}

pub enum WaylandBackendEvent {
    Input(InputEvent<WaylandInputBackend>),
    Frame,
    Close,
    Resize,
}

impl WaylandBackend {
    pub fn new(
        config: Rc<RefCell<Config>>,
        event_loop: LoopHandle<State>,
    ) -> Result<Self, anyhow::Error> {
        let connection = Connection::connect_to_env()?;

        let (globals, queue) = registry_queue_init::<WaylandBackend>(&connection).unwrap();

        let qh = queue.handle();

        event_loop
            .insert_source(WaylandSource::new(connection, queue), |_, queue, state| {
                // This should be the object that we're currently creating.
                // Is there a better way to do this, without a panic path?
                let backend = state.backend.wayland();
                queue.dispatch_pending(backend)
            })
            .unwrap();

        let (events, channel) = calloop::channel::channel();

        event_loop
            .insert_source(channel, |event, _, state| {
                let calloop::channel::Event::Msg(event) = event else {
                    return;
                };
                let niri = &mut state.niri;
                let backend = state.backend.wayland();
                match event {
                    WaylandBackendEvent::Input(event) => state.process_input_event(event),
                    WaylandBackendEvent::Frame => niri.queue_redraw(&backend.output),
                    WaylandBackendEvent::Close => state.do_action(Action::Quit(false), true),
                    WaylandBackendEvent::Resize => {
                        let size = backend.graphics.window_size();
                        debug!("Resizing window to {}x{}", size.w, size.h);
                        backend.output.change_current_state(
                            Some(Mode {
                                size,
                                refresh: 60_000,
                            }),
                            None,
                            None,
                            None,
                        );

                        {
                            let mut ipc_outputs = backend.ipc_outputs.lock().unwrap();
                            let output = ipc_outputs.values_mut().next().unwrap();
                            let mode = &mut output.modes[0];
                            mode.width = size.w.clamp(0, u16::MAX as i32) as u16;
                            mode.height = size.h.clamp(0, u16::MAX as i32) as u16;
                            if let Some(logical) = output.logical.as_mut() {
                                logical.width = size.w as u32;
                                logical.height = size.h as u32;
                            }
                            niri.ipc_outputs_changed = true;
                        }

                        niri.output_resized(&backend.output);
                    }
                }
            })
            .unwrap();

        let registry_state = RegistryState::new(&globals);
        let seat_state = SeatState::new(&globals, &qh);
        let relative_pointer_state = RelativePointerState::bind(&globals, &qh);
        let output_state = OutputState::new(&globals, &qh);
        let compositor_state = CompositorState::bind(&globals, &qh)?;
        let xdg_state = XdgShell::bind(&globals, &qh)?;
        let pointer_constraints_state = PointerConstraintsState::bind(&globals, &qh);

        let output = Output::new(
            "nested niri".to_string(),
            PhysicalProperties {
                size: (0, 0).into(),
                subpixel: Subpixel::Unknown,
                make: "Smithay".into(),
                model: "niri".into(),
            },
        );

        let mode = Mode {
            size: (0, 0).into(),
            refresh: 60_000,
        };
        output.change_current_state(Some(mode), None, None, None);
        output.set_preferred(mode);

        output.user_data().insert_if_missing(|| OutputName {
            connector: "nested".to_string(),
            make: Some("Smithay".to_string()),
            model: Some("niri".to_string()),
            serial: None, // Some("nested".to_string()),
        });

        let physical_properties = output.physical_properties();
        let ipc_outputs = Arc::new(Mutex::new(HashMap::from([(
            OutputId::next(),
            niri_ipc::Output {
                name: output.name(),
                make: physical_properties.make,
                model: physical_properties.model,
                serial: None,
                physical_size: None,
                modes: vec![niri_ipc::Mode {
                    width: 0,
                    height: 0,
                    refresh_rate: 60_000,
                    is_preferred: true,
                }],
                current_mode: Some(0),
                vrr_supported: false,
                vrr_enabled: false,
                logical: Some(logical_output(&output)),
            },
        )])));

        let damage_tracker = OutputDamageTracker::from_output(&output);

        let output_surface = Surface::new(&compositor_state, &qh)?;

        let main_window =
            xdg_state.create_window(output_surface, WindowDecorations::ServerDefault, &qh);

        // This transform is necessary to make the window appear right-side up.
        // It will never change throughout the lifetime of the window.
        main_window
            .set_buffer_transform(wl_output::Transform::Flipped180)
            .unwrap();

        main_window.set_app_id("niri");
        main_window.set_title("niri");

        // We initialize everything as 1x1, and pray the compositor chooses a better size
        // upon the first configure event. This commit is necessary to receive that event.
        main_window.commit();

        let graphics = WaylandGraphicsBackend::new(main_window, (1, 1).into(), &qh)?;

        Ok(Self {
            config,
            events,

            registry_state,
            seat_state,
            relative_pointer_state,
            output_state,
            compositor_state,
            xdg_state,
            pointer_constraints_state,

            locked_pointers: Vec::new(),
            prev_cursor_location: Point::default(),

            output,
            damage_tracker,
            ipc_outputs,

            graphics,
        })
    }

    pub fn send_event(&self, event: WaylandBackendEvent) {
        self.events
            .send(event)
            .expect("WaylandBackend event channel closed");
    }

    pub fn send_input_event(&self, event: InputEvent<WaylandInputBackend>) {
        self.send_event(WaylandBackendEvent::Input(event));
    }

    pub fn set_cursor_position_hint(&mut self, location: Point<f64, Logical>) {
        if location == std::mem::replace(&mut self.prev_cursor_location, location) {
            return;
        }

        // debug!("inner location: {:?}", location,);
        let transform = self.output.current_transform();

        // HACK: ??? the non-flipped variants are fine.
        let transform = match transform {
            Transform::Flipped90 => Transform::Flipped270,
            Transform::Flipped270 => Transform::Flipped90,
            other => other,
        };

        let output_area = transform.transform_size(self.graphics.window_size());

        let location =
            RawAbsolutePosition::new(location.x, location.y, transform.invert(), output_area)
                .position();

        for locked_pointer in &self.locked_pointers {
            locked_pointer.set_cursor_position_hint(location.x, location.y);
        }

        // Adding + 1.0 looks like a hack. It becomes clear why this is done if you consider that
        // pixels are 0-indexed, so 0 resepents the left edge and width-1 represents the right edge.
        // The same goes for the top and bottom edges.
        if location.x == 0.0
            || location.y == 0.0
            || location.x + 1.0 == self.graphics.window_size().w as f64
            || location.y + 1.0 == self.graphics.window_size().h as f64
        {
            for seat in self.seat_state.seats() {
                seat.unlock_pointer(self);
            }
        }
    }

    pub fn init(&mut self, niri: &mut Niri) {
        // This is set to true upon wl_keyboard::enter.
        niri.compositor_has_keyboard_focus = false;
        // This is set to false upon wl_pointer::enter.
        niri.pointer_hidden = true;

        let renderer = self.graphics.renderer();
        if let Err(err) = renderer.bind_wl_display(&niri.display_handle) {
            warn!("error binding renderer wl_display: {err}");
        }

        resources::init(renderer);
        shaders::init(renderer);

        let config = self.config.borrow();
        if let Some(src) = config.animations.window_resize.custom_shader.as_deref() {
            shaders::set_custom_resize_program(renderer, Some(src));
        }
        if let Some(src) = config.animations.window_close.custom_shader.as_deref() {
            shaders::set_custom_close_program(renderer, Some(src));
        }
        if let Some(src) = config.animations.window_open.custom_shader.as_deref() {
            shaders::set_custom_open_program(renderer, Some(src));
        }
        drop(config);

        niri.layout.update_shaders();

        niri.add_output(self.output.clone(), None, false);
    }

    pub fn seat_name(&self) -> String {
        "niri-nested".to_owned()
    }

    pub fn with_primary_renderer<T>(
        &mut self,
        f: impl FnOnce(&mut GlesRenderer) -> T,
    ) -> Option<T> {
        Some(f(self.graphics.renderer()))
    }

    pub fn render(&mut self, niri: &mut Niri, output: &Output) -> RenderResult {
        let _span = tracy_client::span!("WaylandBackend::render");

        // Render the elements.
        let mut elements = niri.render::<GlesRenderer>(
            self.graphics.renderer(),
            output,
            true,
            RenderTarget::Output,
        );

        // Visualize the damage, if enabled.
        if niri.debug_draw_damage {
            let output_state = niri.output_state.get_mut(output).unwrap();
            draw_damage(&mut output_state.debug_damage_tracker, &mut elements);
        }

        // Hand it all over to the graphics backend.
        self.graphics.bind().unwrap();
        let age = self.graphics.buffer_age().unwrap();
        let res = self
            .damage_tracker
            .render_output(self.graphics.renderer(), age, &elements, [0.; 4])
            .unwrap();

        niri.update_primary_scanout_output(output, &res.states);

        let rv;
        if let Some(damage) = res.damage {
            if self
                .config
                .borrow()
                .debug
                .wait_for_frame_completion_before_queueing
            {
                let _span = tracy_client::span!("wait for completion");
                if let Err(err) = res.sync.wait() {
                    warn!("error waiting for frame completion: {err:?}");
                }
            }

            self.graphics
                .submit(damage, output.current_transform())
                .unwrap();

            let mut presentation_feedbacks = niri.take_presentation_feedbacks(output, &res.states);
            let mode = output.current_mode().unwrap();
            let refresh = Duration::from_secs_f64(1_000f64 / mode.refresh as f64);
            presentation_feedbacks.presented::<_, smithay::utils::Monotonic>(
                get_monotonic_time(),
                refresh,
                0,
                wp_presentation_feedback::Kind::empty(),
            );

            rv = RenderResult::Submitted;
        } else {
            rv = RenderResult::NoDamage;
        }

        let output_state = niri.output_state.get_mut(output).unwrap();
        match mem::replace(&mut output_state.redraw_state, RedrawState::Idle) {
            RedrawState::Idle => unreachable!(),
            RedrawState::Queued => (),
            RedrawState::WaitingForVBlank { .. } => unreachable!(),
            RedrawState::WaitingForEstimatedVBlank(_) => unreachable!(),
            RedrawState::WaitingForEstimatedVBlankAndQueued(_) => unreachable!(),
        }

        output_state.frame_callback_sequence = output_state.frame_callback_sequence.wrapping_add(1);

        if output_state.unfinished_animations_remain {
            self.graphics.request_frame_callback();
        }

        rv
    }

    pub fn toggle_debug_tint(&mut self) {
        let renderer = self.graphics.renderer();
        renderer.set_debug_flags(renderer.debug_flags() ^ DebugFlags::TINT);
    }

    pub fn import_dmabuf(&mut self, dmabuf: &Dmabuf) -> bool {
        self.with_primary_renderer(|renderer| match renderer.import_dmabuf(dmabuf, None) {
            Ok(_texture) => true,
            Err(err) => {
                debug!("error importing dmabuf: {err:?}");
                false
            }
        })
        .unwrap_or(false)
    }

    pub fn ipc_outputs(&self) -> Arc<Mutex<IpcOutputMap>> {
        self.ipc_outputs.clone()
    }
}
