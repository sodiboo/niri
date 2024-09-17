#![allow(unused_imports, unused_variables)]

use std::cell::RefCell;
use std::collections::HashMap;
use std::num::NonZeroU32;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use std::{mem, path};

use buffer::WaylandGraphicsBackend;
use calloop::channel::Sender;
use niri_config::{Config, OutputName};
use smithay::backend::allocator::dmabuf::Dmabuf;
use smithay::backend::allocator::gbm::GbmDevice;
use smithay::backend::allocator::Fourcc;
use smithay::backend::egl::{EGLDevice, EGLDisplay};
use smithay::backend::renderer::damage::OutputDamageTracker;
use smithay::backend::renderer::gles::GlesRenderer;
use smithay::backend::renderer::{DebugFlags, ImportDma, ImportEgl, Renderer};
use smithay::backend::winit::{self, WinitEvent, WinitEventLoop, WinitGraphicsBackend};
use smithay::output::{Mode, Output, PhysicalProperties, Subpixel};
use smithay::reexports::calloop::LoopHandle;
use smithay::reexports::gbm::Format;
use smithay::reexports::wayland_protocols::wp::presentation_time::server::wp_presentation_feedback;
use smithay::utils::{Physical, Size};
use smithay_client_toolkit::compositor::{CompositorState, Surface};
use smithay_client_toolkit::dmabuf::{DmabufFeedback, DmabufState};
use smithay_client_toolkit::output::OutputState;
use smithay_client_toolkit::reexports::calloop_wayland_source::WaylandSource;
use smithay_client_toolkit::reexports::client::globals::registry_queue_init;
use smithay_client_toolkit::reexports::client::protocol::wl_shm;
use smithay_client_toolkit::reexports::client::protocol::wl_surface::WlSurface;
use smithay_client_toolkit::reexports::client::{Connection, EventQueue, QueueHandle};
use smithay_client_toolkit::reexports::csd_frame::WindowState;
use smithay_client_toolkit::registry::RegistryState;
use smithay_client_toolkit::shell::xdg::window::{Window, WindowDecorations};
use smithay_client_toolkit::shell::xdg::XdgShell;
use smithay_client_toolkit::shell::WaylandSurface;
use smithay_client_toolkit::shm::Shm;

use super::{IpcOutputMap, OutputId, RenderResult};
use crate::niri::{Niri, RedrawState, State};
use crate::render_helpers::debug::draw_damage;
use crate::render_helpers::{resources, shaders, RenderTarget};
use crate::utils::{get_monotonic_time, logical_output};

mod buffer;
mod graphics;
mod handlers;

pub struct WaylandBackend {
    config: Rc<RefCell<Config>>,
    events: Sender<WaylandBackendEvent>,

    qh: QueueHandle<Self>,

    registry_state: RegistryState,
    shm_state: Shm,
    output_state: OutputState,
    compositor_state: CompositorState,
    xdg_state: XdgShell,
    dmabuf_state: DmabufState,

    output: Output,
    output_size: Size<u32, Physical>,
    // graphics: WinitGraphicsBackend<GlesRenderer>,
    damage_tracker: OutputDamageTracker,
    ipc_outputs: Arc<Mutex<IpcOutputMap>>,

    dmabuf_feedback: Option<DmabufFeedback>,

    main_window: Window,

    graphics: Option<WaylandGraphicsBackend>,
}

pub enum WaylandBackendEvent {
    CloseRequest,
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
                match event {
                    WaylandBackendEvent::CloseRequest => {
                        state.niri.stop_signal.stop();
                    }
                }
            })
            .unwrap();

        let registry_state = RegistryState::new(&globals);
        let shm_state = Shm::bind(&globals, &qh)?;
        let output_state = OutputState::new(&globals, &qh);
        let compositor_state = CompositorState::bind(&globals, &qh)?;
        let xdg_state = XdgShell::bind(&globals, &qh)?;
        let dmabuf_state = DmabufState::new(&globals, &qh);

        let output = Output::new(
            "nested niri".to_string(),
            PhysicalProperties {
                size: (1280, 720).into(),
                subpixel: Subpixel::Unknown,
                make: "Smithay".into(),
                model: "niri".into(),
            },
        );

        output.user_data().insert_if_missing(|| OutputName {
            connector: "nested".to_string(),
            make: Some("Smithay".to_string()),
            model: Some("niri".to_string()),
            serial: None, // Some("nested".to_string()),
        });

        let physical_properties = output.physical_properties();
        let ipc_outputs = Arc::new(Mutex::new(HashMap::from([
        //     (
        //     OutputId::next(),
        //     niri_ipc::Output {
        //         name: output.name(),
        //         make: physical_properties.make,
        //         model: physical_properties.model,
        //         serial: None,
        //         physical_size: None,
        //         modes: vec![niri_ipc::Mode {
        //             width: 0,
        //             height: 0,
        //             refresh_rate: 60_000,
        //             is_preferred: true,
        //         }],
        //         current_mode: Some(0),
        //         vrr_supported: false,
        //         vrr_enabled: false,
        //         logical: Some(logical_output(&output)),
        //     },
        // )
        ])));

        let damage_tracker = OutputDamageTracker::from_output(&output);

        let output_surface = Surface::new(&compositor_state, &qh)?;

        let main_window =
            xdg_state.create_window(output_surface, WindowDecorations::ServerDefault, &qh);

        main_window.commit();

        Ok(Self {
            config,
            events,

            qh,

            registry_state,
            shm_state,
            output_state,
            compositor_state,
            xdg_state,
            dmabuf_state,

            output,
            damage_tracker,
            ipc_outputs,

            dmabuf_feedback: None,

            output_size: (0, 0).into(),

            main_window,

            graphics: None,
        })
    }

    fn got_gbm_device(
        &mut self,
        path: PathBuf,
        gbm: GbmDevice<std::fs::File>,
        feedback: DmabufFeedback,
    ) {
        info!("Got gbm device from dmabuf feedback");
        let surface = self.create_gbm_buffer(
            gbm,
            feedback,
            Fourcc::Abgr8888,
            (self.output_size.w, self.output_size.h),
            false,
        );

        match surface {
            Ok(Some(buf)) => {
                info!("got Ok(Some(_))");
                self.main_window.attach(Some(&buf.buffer), 0, 0);

                self.graphics = Some(buf);
            }
            Ok(None) => info!("got Ok(None)"),
            Err(err) => error!("err: {err:?}"),
        }
    }

    fn failed_gbm_device(&mut self, feedback: DmabufFeedback) {}

    pub fn init(&mut self, niri: &mut Niri) {
        //     let renderer = self.backend.renderer();
        //     if let Err(err) = renderer.bind_wl_display(&niri.display_handle) {
        //         warn!("error binding renderer wl_display: {err}");
        //     }

        //     resources::init(renderer);
        //     shaders::init(renderer);

        //     let config = self.config.borrow();
        //     if let Some(src) = config.animations.window_resize.custom_shader.as_deref() {
        //         shaders::set_custom_resize_program(renderer, Some(src));
        //     }
        //     if let Some(src) = config.animations.window_close.custom_shader.as_deref() {
        //         shaders::set_custom_close_program(renderer, Some(src));
        //     }
        //     if let Some(src) = config.animations.window_open.custom_shader.as_deref() {
        //         shaders::set_custom_open_program(renderer, Some(src));
        //     }
        //     drop(config);

        //     niri.layout.update_shaders();

        //     niri.add_output(self.output.clone(), None, false);
    }

    pub fn seat_name(&self) -> String {
        "winit".to_owned()
    }

    pub fn with_primary_renderer<T>(
        &mut self,
        f: impl FnOnce(&mut GlesRenderer) -> T,
    ) -> Option<T> {
        todo!()
        // Some(f(self.backend.renderer()))
    }

    pub fn render(&mut self, niri: &mut Niri, output: &Output) -> RenderResult {
        // let _span = tracy_client::span!("Wayland::render");

        // // Render the elements.
        // let mut elements = niri.render::<GlesRenderer>(
        //     self.backend.renderer(),
        //     output,
        //     true,
        //     RenderTarget::Output,
        // );

        // // Visualize the damage, if enabled.
        // if niri.debug_draw_damage {
        //     let output_state = niri.output_state.get_mut(output).unwrap();
        //     draw_damage(&mut output_state.debug_damage_tracker, &mut elements);
        // }

        // // Hand them over to winit.
        // self.backend.bind().unwrap();
        // let age = self.backend.buffer_age().unwrap();
        // let res = self
        //     .damage_tracker
        //     .render_output(self.backend.renderer(), age, &elements, [0.; 4])
        //     .unwrap();

        // niri.update_primary_scanout_output(output, &res.states);

        // let rv;
        // if let Some(damage) = res.damage {
        //     if self
        //         .config
        //         .borrow()
        //         .debug
        //         .wait_for_frame_completion_before_queueing
        //     {
        //         let _span = tracy_client::span!("wait for completion");
        //         if let Err(err) = res.sync.wait() {
        //             warn!("error waiting for frame completion: {err:?}");
        //         }
        //     }

        //     self.backend.submit(Some(damage)).unwrap();

        //     let mut presentation_feedbacks = niri.take_presentation_feedbacks(output,
        // &res.states);     let mode = output.current_mode().unwrap();
        //     let refresh = Duration::from_secs_f64(1_000f64 / mode.refresh as f64);
        //     presentation_feedbacks.presented::<_, smithay::utils::Monotonic>(
        //         get_monotonic_time(),
        //         refresh,
        //         0,
        //         wp_presentation_feedback::Kind::empty(),
        //     );

        //     rv = RenderResult::Submitted;
        // } else {
        //     rv = RenderResult::NoDamage;
        // }

        // let output_state = niri.output_state.get_mut(output).unwrap();
        // match mem::replace(&mut output_state.redraw_state, RedrawState::Idle) {
        //     RedrawState::Idle => unreachable!(),
        //     RedrawState::Queued => (),
        //     RedrawState::WaitingForVBlank { .. } => unreachable!(),
        //     RedrawState::WaitingForEstimatedVBlank(_) => unreachable!(),
        //     RedrawState::WaitingForEstimatedVBlankAndQueued(_) => unreachable!(),
        // }

        // output_state.frame_callback_sequence =
        // output_state.frame_callback_sequence.wrapping_add(1);

        // // FIXME: this should wait until a frame callback from the host compositor, but it
        // redraws // right away instead.
        // if output_state.unfinished_animations_remain {
        //     self.backend.window().request_redraw();
        // }

        // rv

        todo!()
    }

    pub fn toggle_debug_tint(&mut self) {
        // let renderer = self.backend.renderer();
        // renderer.set_debug_flags(renderer.debug_flags() ^ DebugFlags::TINT);
    }

    pub fn import_dmabuf(&mut self, dmabuf: &Dmabuf) -> bool {
        // match self.backend.renderer().import_dmabuf(dmabuf, None) {
        //     Ok(_texture) => true,
        //     Err(err) => {
        //         debug!("error importing dmabuf: {err:?}");
        //         false
        //     }
        // }
        todo!()
    }

    pub fn ipc_outputs(&self) -> Arc<Mutex<IpcOutputMap>> {
        self.ipc_outputs.clone()
    }
}
