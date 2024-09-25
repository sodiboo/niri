use std::sync::Mutex;

use smithay::backend::input;
use smithay::backend::input::{ButtonState, InputEvent};
use smithay_client_toolkit::reexports::client::globals::GlobalList;
use smithay_client_toolkit::reexports::client::protocol::wl_keyboard::{self, WlKeyboard};
use smithay_client_toolkit::reexports::client::protocol::wl_pointer::{self, WlPointer};
use smithay_client_toolkit::reexports::client::protocol::wl_seat::{self, WlSeat};
use smithay_client_toolkit::reexports::client::protocol::wl_surface::WlSurface;
use smithay_client_toolkit::reexports::client::protocol::wl_touch::{self, WlTouch};
use smithay_client_toolkit::reexports::client::{Connection, Dispatch, Proxy, QueueHandle};
use smithay_client_toolkit::reexports::protocols::wp::pointer_constraints::zv1::client::zwp_confined_pointer_v1::ZwpConfinedPointerV1;
use smithay_client_toolkit::reexports::protocols::wp::pointer_constraints::zv1::client::zwp_locked_pointer_v1::ZwpLockedPointerV1;
use smithay_client_toolkit::reexports::protocols::wp::pointer_constraints::zv1::client::zwp_pointer_constraints_v1::Lifetime;
use smithay_client_toolkit::reexports::protocols::wp::relative_pointer::zv1::client::zwp_relative_pointer_v1::ZwpRelativePointerV1;
use smithay_client_toolkit::registry::{ProvidesRegistryState, RegistryHandler};
use smithay_client_toolkit::seat::pointer_constraints::PointerConstraintsHandler;
use smithay_client_toolkit::seat::relative_pointer::{RelativeMotionEvent, RelativePointerHandler};
use smithay_client_toolkit::shell::WaylandSurface;
use wayland_backend::protocol::WEnum;

use super::input::{
    AxisFrame, WaylandInputSpecialEvent, WaylandKeyboardEvent, WaylandPointerAxisEvent,
    WaylandPointerButtonEvent, WaylandPointerMotionEvent, WaylandPointerRelativeMotionEvent,
    WaylandTouchCancelEvent, WaylandTouchDownEvent, WaylandTouchFrameEvent,
    WaylandTouchMotionEvent, WaylandTouchUpEvent,
};
use super::WaylandBackend;

#[derive(Debug)]
pub struct SeatState {
    // (name, seat)
    seats: Vec<SeatInner>,
}

#[derive(Debug)]
struct SeatInner {
    seat: Seat,
    name: u32,
}

#[derive(Debug, Clone)]
struct Seat {
    seat: WlSeat,
}

impl Seat {
    fn data(&self) -> &SeatData {
        self.seat.data().expect("WlSeat has no SeatData")
    }

    fn lock_pointer(
        &self,
        surface: &WlSurface,
        backend: &mut WaylandBackend,
        qh: &QueueHandle<WaylandBackend>,
    ) {
        self.data().with_devices_mut(|devices| {
            let Some(pointer) = &devices.pointer else {
                return;
            };

            // Don't bother with a pointer lock if we don't get relative pointer events.
            // Does there event exist a compositor that implements pointer constraints
            // but not relative pointer?
            if devices.relative_pointer.is_none() {
                return;
            }

            devices.locked_pointer = devices.locked_pointer.take().or_else(|| {
                backend
                    .pointer_constraints_state
                    .lock_pointer(surface, pointer, None, Lifetime::Persistent, qh)
                    .inspect(|locked_pointer| {
                        backend.locked_pointers.push(locked_pointer.clone());
                    })
                    .ok()
            });
        })
    }

    fn unlock_pointer(&self, backend: &mut WaylandBackend) {
        self.data().with_devices_mut(|devices| {
            if let Some(locked_pointer) = devices.locked_pointer.take() {
                locked_pointer.destroy();
                backend.locked_pointers.retain(|p| p != &locked_pointer);
            }
        })
    }
}

/// Serves to own as many input devices as possible, for the sole purpose of receiving the
/// appropriate events.
#[derive(Debug, Default)]
struct SeatDevices {
    keyboard: Option<WlKeyboard>,
    pointer: Option<WlPointer>,
    relative_pointer: Option<ZwpRelativePointerV1>,
    locked_pointer: Option<ZwpLockedPointerV1>,
    touch: Option<WlTouch>,
}

impl RegistryHandler<WaylandBackend> for SeatState {
    fn new_global(
        backend: &mut WaylandBackend,
        _: &Connection,
        qh: &QueueHandle<WaylandBackend>,
        name: u32,
        interface: &str,
        _: u32,
    ) {
        if interface == WlSeat::interface().name {
            let seat = backend
                .registry()
                .bind_specific(qh, name, 1..=7, SeatData::default())
                .expect("failed to bind global");

            let seat = Seat { seat };

            backend.seat_state.seats.push(SeatInner { seat, name });
        }
    }

    fn remove_global(
        backend: &mut WaylandBackend,
        _: &Connection,
        _: &QueueHandle<WaylandBackend>,
        name: u32,
        interface: &str,
    ) {
        if interface == WlSeat::interface().name {
            if let Some(seat) = backend
                .seat_state
                .seats
                .iter()
                .find_map(|inner| (inner.name == name).then_some(inner.seat.clone()))
            {
                seat.data().with_devices_mut(|devices| {
                    if let Some(keyboard) = devices.keyboard.take() {
                        keyboard.release();
                        backend.send_input_event(InputEvent::DeviceRemoved {
                            device: keyboard.into(),
                        });
                    }
                    if let Some(locked_pointer) = devices.locked_pointer.take() {
                        locked_pointer.destroy();
                        backend.locked_pointers.retain(|p| p != &locked_pointer);
                    }
                    if let Some(relative_pointer) = devices.relative_pointer.take() {
                        relative_pointer.destroy();
                    }
                    if let Some(pointer) = devices.pointer.take() {
                        pointer.release();
                        backend.send_input_event(InputEvent::DeviceRemoved {
                            device: pointer.into(),
                        });
                    }

                    if let Some(touch) = devices.touch.take() {
                        touch.release();
                        backend.send_input_event(InputEvent::DeviceRemoved {
                            device: touch.into(),
                        });
                    }
                });

                backend.seat_state.seats.retain(|inner| inner.name != name);
            }
        }
    }
}

const SEAT_VERSION: u32 = 7;

impl SeatState {
    pub fn new(global_list: &GlobalList, qh: &QueueHandle<WaylandBackend>) -> SeatState {
        // smithay_client_toolkit::registry::bind_all is private
        // but by inlining it here, this function is actually a lot nicer lol.
        global_list.contents().with_list(|globals| {
            assert!(SEAT_VERSION <= WlSeat::interface().version);
            SeatState {
                seats: globals
                    .iter()
                    .filter(|global| global.interface == WlSeat::interface().name)
                    .map(|global| {
                        let version = global.version.min(SEAT_VERSION);
                        let name = global.name;
                        let seat: WlSeat = global_list.registry().bind(
                            global.name,
                            version,
                            qh,
                            SeatData::default(),
                        );
                        let seat = Seat { seat };
                        SeatInner { seat, name }
                    })
                    .collect(),
            }
        })
    }
}

#[derive(Debug, Default)]
struct SeatData {
    devices: Mutex<SeatDevices>,
}

impl SeatData {
    fn with_devices_mut<T>(&self, f: impl FnOnce(&mut SeatDevices) -> T) -> T {
        f(&mut self.devices.lock().unwrap())
    }
}

impl Dispatch<WlSeat, SeatData> for WaylandBackend {
    fn event(
        backend: &mut Self,
        seat: &WlSeat,
        event: wl_seat::Event,
        data: &SeatData,
        _: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        match event {
            wl_seat::Event::Name { name } => {
                // we don't care about the name lol
                debug!("Seat name: {name}");
            }
            wl_seat::Event::Capabilities { capabilities } => {
                let capabilities = wl_seat::Capability::from_bits_truncate(capabilities.into());

                data.with_devices_mut(|devices| {
                    if capabilities.contains(wl_seat::Capability::Keyboard) {
                        devices.keyboard.get_or_insert_with(|| {
                            let keyboard = seat.get_keyboard(qh, Seat { seat: seat.clone() });
                            backend.send_input_event(InputEvent::DeviceAdded {
                                device: keyboard.clone().into(),
                            });
                            keyboard
                        });
                    } else if let Some(keyboard) = devices.keyboard.take() {
                        keyboard.release();
                        backend.send_input_event(InputEvent::DeviceRemoved {
                            device: keyboard.into(),
                        });
                    }

                    if capabilities.contains(wl_seat::Capability::Pointer) {
                        let pointer = devices.pointer.get_or_insert_with(|| {
                            let pointer = seat.get_pointer(qh, PointerData::default());
                            backend.send_input_event(InputEvent::DeviceAdded {
                                device: pointer.clone().into(),
                            });
                            pointer
                        });

                        devices.relative_pointer = devices.relative_pointer.take().or_else(|| {
                            backend
                                .relative_pointer_state
                                .get_relative_pointer(pointer, qh)
                                .ok()
                        });
                    } else {
                        if let Some(relative_pointer) = devices.relative_pointer.take() {
                            relative_pointer.destroy();
                        }
                        if let Some(pointer) = devices.pointer.take() {
                            pointer.release();
                            backend.send_input_event(InputEvent::DeviceRemoved {
                                device: pointer.into(),
                            });
                        }
                    }

                    if capabilities.contains(wl_seat::Capability::Touch) {
                        devices.touch.get_or_insert_with(|| {
                            let touch = seat.get_touch(qh, ());
                            backend.send_input_event(InputEvent::DeviceAdded {
                                device: touch.clone().into(),
                            });
                            touch
                        });
                    } else if let Some(touch) = devices.touch.take() {
                        touch.release();
                        backend.send_input_event(InputEvent::DeviceRemoved {
                            device: touch.into(),
                        });
                    }
                });
            }
            _ => unreachable!(),
        }
    }
}

impl Dispatch<WlKeyboard, Seat> for WaylandBackend {
    fn event(
        backend: &mut Self,
        keyboard: &WlKeyboard,
        event: <WlKeyboard as Proxy>::Event,
        seat: &Seat,
        _: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        let keyboard = keyboard.clone();
        match event {
            wl_keyboard::Event::Keymap { format, fd, size } => {
                assert_eq!(format, WEnum::Value(wl_keyboard::KeymapFormat::XkbV1));
                backend.send_input_event(InputEvent::Special(
                    WaylandInputSpecialEvent::KeyboardKeymap { keyboard, fd, size },
                ));
            }
            wl_keyboard::Event::Enter {
                serial,
                surface,
                keys,
            } => {
                assert_eq!(&surface, backend.graphics.window().wl_surface());
                // Keysyms are encoded as an array of u32
                let raw = keys
                    .chunks_exact(4)
                    .flat_map(TryInto::<[u8; 4]>::try_into)
                    .map(u32::from_le_bytes)
                    .collect::<Vec<_>>();

                backend.send_input_event(InputEvent::Special(
                    WaylandInputSpecialEvent::KeyboardEnter {
                        keyboard,
                        serial,
                        keys: raw.into_iter().collect(),
                    },
                ));
                seat.lock_pointer(&surface, backend, qh);
            }
            wl_keyboard::Event::Leave { serial, surface } => {
                assert_eq!(&surface, backend.graphics.window().wl_surface());
                backend.send_input_event(InputEvent::Special(
                    WaylandInputSpecialEvent::KeyboardLeave { keyboard, serial },
                ));
                seat.unlock_pointer(backend);
            }
            wl_keyboard::Event::Key {
                serial,
                time,
                key,
                state,
            } => {
                let state = match state.into_result().unwrap() {
                    wl_keyboard::KeyState::Pressed => input::KeyState::Pressed,
                    wl_keyboard::KeyState::Released => input::KeyState::Released,
                    _ => unreachable!(),
                };
                backend.send_input_event(InputEvent::Keyboard {
                    event: WaylandKeyboardEvent {
                        keyboard,
                        serial,
                        time,
                        key,
                        state,
                    },
                });
            }
            wl_keyboard::Event::Modifiers {
                serial,
                mods_depressed,
                mods_latched,
                mods_locked,
                group,
            } => {
                backend.send_input_event(InputEvent::Special(
                    WaylandInputSpecialEvent::KeyboardModifiers {
                        keyboard,
                        serial,
                        depressed: mods_depressed,
                        latched: mods_latched,
                        locked: mods_locked,
                        group,
                    },
                ));
            }
            wl_keyboard::Event::RepeatInfo { rate, delay } => {
                backend.send_input_event(InputEvent::Special(
                    WaylandInputSpecialEvent::KeyboardRepeatInfo {
                        keyboard,
                        rate,
                        delay,
                    },
                ));
            }
            _ => unreachable!(),
        }
    }
}

#[derive(Debug, Default)]
struct PointerData {
    axis_frame: Mutex<AxisFrame>,
}

impl PointerData {
    fn with_axis_frame_mut<T>(&self, f: impl FnOnce(&mut AxisFrame) -> T) -> T {
        f(&mut self.axis_frame.lock().unwrap())
    }
}

impl Dispatch<WlPointer, PointerData> for WaylandBackend {
    fn event(
        backend: &mut Self,
        proxy: &WlPointer,
        event: <WlPointer as Proxy>::Event,
        data: &PointerData,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        let pointer = proxy.clone();

        // FIXME: For `PointerEventKind::Enter`, we're supposed to also
        // use `event.position` to determine the position of the pointer.
        // In particular, a pointer enter can (and *will*) be sent without a motion event,
        // so we shouldn't rely on the motion event to refresh the cursor upon entering.
        //
        // As it stands, if the cursor enters our surface without moving, we hide the external
        // cursor but don't show our own cursor. That's not great, as it leads to the cursor
        // being invisible.
        //
        // But it also requires frame-perfect user input to trigger.
        // In practice, this doesn't cause any issues, because
        // you'll only experience it if you're looking for it.
        //
        // To fix this, `PointerEventKind::Enter` should
        // emit `InputEvent::PointerMotionAbsolute` but that event requires
        // a `time` value, which we don't get on `PointerEventKind::Enter`.
        // And this `time` value is quite important, so it's nontrivial to make one up.
        // Therefore, there is no easy way to send that event correctly.
        //
        // It's also just annoying to modify the below code to send two events,
        // so that alone is reason enough to not fix this for now.

        match event {
            wl_pointer::Event::Enter {
                serial,
                surface,
                surface_x,
                surface_y,
            } => {
                assert_eq!(&surface, backend.graphics.window().wl_surface());
                backend.send_input_event(InputEvent::Special(
                    WaylandInputSpecialEvent::PointerEnter {
                        pointer,
                        serial,
                        surface_x,
                        surface_y,
                    },
                ));
            }
            wl_pointer::Event::Leave { serial, surface } => {
                assert_eq!(&surface, backend.graphics.window().wl_surface());
                backend.send_input_event(InputEvent::Special(
                    WaylandInputSpecialEvent::PointerLeave { pointer, serial },
                ));
            }
            wl_pointer::Event::Motion {
                time,
                surface_x,
                surface_y,
            } => {
                backend.send_input_event(InputEvent::PointerMotionAbsolute {
                    event: WaylandPointerMotionEvent {
                        pointer,
                        time,
                        x: surface_x,
                        y: surface_y,
                        window_size: backend.graphics.window_size(),
                    },
                });
            }
            wl_pointer::Event::Button {
                serial,
                time,
                button,
                state,
            } => {
                let state = match state.into_result().unwrap() {
                    wl_pointer::ButtonState::Pressed => ButtonState::Pressed,
                    wl_pointer::ButtonState::Released => ButtonState::Released,
                    _ => unreachable!(),
                };
                backend.send_input_event(InputEvent::PointerButton {
                    event: WaylandPointerButtonEvent {
                        pointer,
                        serial,
                        time,
                        button,
                        state,
                    },
                });
            }
            wl_pointer::Event::Axis { time, axis, value } => {
                let axis = match axis.into_result().unwrap() {
                    wl_pointer::Axis::VerticalScroll => input::Axis::Vertical,
                    wl_pointer::Axis::HorizontalScroll => input::Axis::Horizontal,
                    _ => unreachable!(),
                };

                data.with_axis_frame_mut(|axis_frame| {
                    axis_frame.time(time);
                    axis_frame[axis].absolute += value;
                })
            }
            wl_pointer::Event::Frame => {
                let axis_frame = data.with_axis_frame_mut(std::mem::take);
                backend.send_input_event(InputEvent::PointerAxis {
                    event: WaylandPointerAxisEvent {
                        pointer,
                        axis_frame,
                    },
                });
            }
            wl_pointer::Event::AxisSource { axis_source } => {
                let source = match axis_source.into_result().unwrap() {
                    wl_pointer::AxisSource::Wheel => input::AxisSource::Wheel,
                    wl_pointer::AxisSource::Finger => input::AxisSource::Finger,
                    wl_pointer::AxisSource::Continuous => input::AxisSource::Continuous,
                    wl_pointer::AxisSource::WheelTilt => input::AxisSource::WheelTilt,
                    _ => unreachable!(),
                };

                data.with_axis_frame_mut(|axis_frame| axis_frame.source = source)
            }
            wl_pointer::Event::AxisStop { time, axis } => {
                let axis = match axis.into_result().unwrap() {
                    wl_pointer::Axis::VerticalScroll => input::Axis::Vertical,
                    wl_pointer::Axis::HorizontalScroll => input::Axis::Horizontal,
                    _ => unreachable!(),
                };

                // We don't actually have an InputEvent interpretation of AxisStop.
                // So just set the time and ignore the stop, lol.
                data.with_axis_frame_mut(|axis_frame| axis_frame.time(time));
                let _ = axis;
            }
            wl_pointer::Event::AxisDiscrete { axis, discrete } => {
                let axis = match axis.into_result().unwrap() {
                    wl_pointer::Axis::VerticalScroll => input::Axis::Vertical,
                    wl_pointer::Axis::HorizontalScroll => input::Axis::Horizontal,
                    _ => unreachable!(),
                };

                data.with_axis_frame_mut(|axis_frame| axis_frame[axis].v120 += discrete * 120)
            }
            wl_pointer::Event::AxisValue120 { axis, value120 } => {
                let axis = match axis.into_result().unwrap() {
                    wl_pointer::Axis::VerticalScroll => input::Axis::Vertical,
                    wl_pointer::Axis::HorizontalScroll => input::Axis::Horizontal,
                    _ => unreachable!(),
                };

                data.with_axis_frame_mut(|axis_frame| axis_frame[axis].v120 += value120)
            }
            wl_pointer::Event::AxisRelativeDirection { axis, direction } => {
                let axis = match axis.into_result().unwrap() {
                    wl_pointer::Axis::VerticalScroll => input::Axis::Vertical,
                    wl_pointer::Axis::HorizontalScroll => input::Axis::Horizontal,
                    _ => unreachable!(),
                };
                let direction = match direction.into_result().unwrap() {
                    wl_pointer::AxisRelativeDirection::Identical => {
                        input::AxisRelativeDirection::Identical
                    }
                    wl_pointer::AxisRelativeDirection::Inverted => {
                        input::AxisRelativeDirection::Inverted
                    }
                    _ => unreachable!(),
                };

                data.with_axis_frame_mut(|axis_frame| {
                    axis_frame[axis].relative_direction = direction
                })
            }
            _ => todo!(),
        }

        if proxy.version() < 5 {
            let axis_frame = data.with_axis_frame_mut(std::mem::take);

            if axis_frame != AxisFrame::default() {
                backend.send_input_event(InputEvent::PointerAxis {
                    event: WaylandPointerAxisEvent {
                        pointer: proxy.clone(),
                        axis_frame,
                    },
                });
            }
        }
    }
}

impl RelativePointerHandler for WaylandBackend {
    fn relative_pointer_motion(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &ZwpRelativePointerV1,
        pointer: &wl_pointer::WlPointer,
        event: RelativeMotionEvent,
    ) {
        self.send_input_event(InputEvent::PointerMotion {
            event: WaylandPointerRelativeMotionEvent {
                pointer: pointer.clone(),
                relative_motion: event,
            },
        })
    }
}

smithay_client_toolkit::delegate_relative_pointer!(WaylandBackend);

impl PointerConstraintsHandler for WaylandBackend {
    fn confined(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &ZwpConfinedPointerV1,
        _: &WlSurface,
        _: &WlPointer,
    ) {
    }

    fn unconfined(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &ZwpConfinedPointerV1,
        _: &WlSurface,
        _: &WlPointer,
    ) {
    }

    fn locked(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &ZwpLockedPointerV1,
        _: &WlSurface,
        _: &WlPointer,
    ) {
    }

    fn unlocked(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &ZwpLockedPointerV1,
        _: &WlSurface,
        _: &WlPointer,
    ) {
    }
}

smithay_client_toolkit::delegate_pointer_constraints!(WaylandBackend);

impl Dispatch<WlTouch, ()> for WaylandBackend {
    fn event(
        backend: &mut Self,
        touch: &WlTouch,
        event: <WlTouch as Proxy>::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        let touch = touch.clone();
        match event {
            wl_touch::Event::Down {
                serial,
                time,
                surface,
                id,
                x,
                y,
            } => {
                assert_eq!(&surface, backend.graphics.window().wl_surface());
                backend.send_input_event(InputEvent::TouchDown {
                    event: WaylandTouchDownEvent {
                        touch,
                        time,
                        slot: Some(id as u32).into(),
                        x,
                        y,
                        window_size: backend.graphics.window_size(),
                        serial,
                    },
                });
            }
            wl_touch::Event::Up { serial, time, id } => {
                backend.send_input_event(InputEvent::TouchUp {
                    event: WaylandTouchUpEvent {
                        touch,
                        time,
                        slot: Some(id as u32).into(),
                        serial,
                    },
                });
            }
            wl_touch::Event::Motion { time, id, x, y } => {
                backend.send_input_event(InputEvent::TouchMotion {
                    event: WaylandTouchMotionEvent {
                        touch,
                        time,
                        slot: Some(id as u32).into(),
                        x,
                        y,
                        window_size: backend.graphics.window_size(),
                    },
                });
            }
            wl_touch::Event::Frame => {
                backend.send_input_event(InputEvent::TouchFrame {
                    event: WaylandTouchFrameEvent {
                        touch,
                        // There's no time field in the Frame event. But niri doesn't use the time
                        // value for this event anyways, so it's fine.
                        time: 0,
                    },
                });
            }
            wl_touch::Event::Cancel => {
                backend.send_input_event(InputEvent::TouchCancel {
                    event: WaylandTouchCancelEvent {
                        touch,
                        // ditto, niri also doesn't use this one
                        time: 0,
                        slot: None.into(),
                    },
                });
            }
            wl_touch::Event::Shape { .. } => {
                // InputEvent can't handle WlTouch::Shape.
            }
            wl_touch::Event::Orientation { .. } => {
                // InputEvent can't handle WlTouch::Orientation.
            }
            _ => todo!(),
        }
    }
}
