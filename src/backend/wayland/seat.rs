use smithay::backend::input;
use smithay::backend::input::{ButtonState, InputEvent, KeyState};
use smithay::input::keyboard::Keysym;
use smithay::reexports::wayland_protocols::wp::relative_pointer::zv1::client::zwp_relative_pointer_v1::ZwpRelativePointerV1;
use smithay_client_toolkit::reexports::client::protocol::wl_keyboard::WlKeyboard;
use smithay_client_toolkit::reexports::client::protocol::wl_pointer::{self, WlPointer};
use smithay_client_toolkit::reexports::client::protocol::wl_seat::WlSeat;
use smithay_client_toolkit::reexports::client::protocol::wl_surface::WlSurface;
use smithay_client_toolkit::reexports::client::protocol::wl_touch::WlTouch;
use smithay_client_toolkit::reexports::client::{Connection, QueueHandle};
use smithay_client_toolkit::seat::relative_pointer::RelativePointerHandler;
use smithay_client_toolkit::seat::touch::TouchHandler;
use smithay_client_toolkit::seat;
use smithay_client_toolkit::seat::keyboard::{KeyboardHandler, Modifiers};
use smithay_client_toolkit::seat::pointer::{PointerEventKind, PointerHandler};
use smithay_client_toolkit::seat::SeatHandler;
use smithay_client_toolkit::shell::WaylandSurface;

use crate::backend::wayland::input::WaylandTouchDownEvent;

use super::input::{WaylandInputDevice, WaylandInputSpecialEvent, WaylandKeyboardEvent, WaylandPointerAxisEvent, WaylandPointerButtonEvent, WaylandPointerMotionEvent, WaylandPointerRelativeMotionEvent, WaylandTouchCancelEvent, WaylandTouchMotionEvent, WaylandTouchUpEvent};
use super::WaylandBackend;

/// Serves to own as many input devices as possible, for the sole purpose of receiving the
/// appropriate events.
pub struct SeatDevices {
    keyboard: Option<WlKeyboard>,
    pointer: Option<WlPointer>,
    relative_pointer: Option<ZwpRelativePointerV1>,
    touch: Option<WlTouch>,
}

impl SeatHandler for WaylandBackend {
    fn seat_state(&mut self) -> &mut seat::SeatState {
        &mut self.seat_state
    }

    fn new_seat(&mut self, _: &Connection, _: &QueueHandle<Self>, _: WlSeat) {}

    fn new_capability(
        &mut self,
        _: &Connection,
        qh: &QueueHandle<Self>,
        seat: WlSeat,
        capability: seat::Capability,
    ) {
        let capabilities = self.seats.entry(seat.clone()).or_insert(SeatDevices {
            keyboard: None,
            pointer: None,
            relative_pointer: None,
            touch: None,
        });

        match capability {
            seat::Capability::Keyboard => {
                let keyboard = self.seat_state.get_keyboard(qh, &seat, None).unwrap();
                capabilities.keyboard = Some(keyboard.clone());
                self.send_input_event(InputEvent::DeviceAdded {
                    device: WaylandInputDevice::Keyboard(keyboard),
                });
            }
            seat::Capability::Pointer => {
                let pointer = self.seat_state.get_pointer(qh, &seat).unwrap();
                capabilities.pointer = Some(pointer.clone());
                capabilities.relative_pointer = self
                    .relative_pointer_state
                    .get_relative_pointer(&pointer, qh)
                    .ok();
                self.send_input_event(InputEvent::DeviceAdded {
                    device: WaylandInputDevice::Pointer(pointer),
                });
            }
            seat::Capability::Touch => {
                let touch = self.seat_state.get_touch(qh, &seat).unwrap();
                capabilities.touch = Some(touch.clone());
                self.send_input_event(InputEvent::DeviceAdded {
                    device: WaylandInputDevice::Touch(touch),
                });
            }
            _ => warn!("Unknown capability {capability}"),
        }
    }

    fn remove_capability(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        seat: WlSeat,
        capability: seat::Capability,
    ) {
        let capabilities = self.seats.entry(seat).or_insert(SeatDevices {
            keyboard: None,
            pointer: None,
            relative_pointer: None,
            touch: None,
        });

        match capability {
            seat::Capability::Keyboard => {
                if let Some(keyboard) = capabilities.keyboard.take() {
                    keyboard.release();
                    self.send_input_event(InputEvent::DeviceRemoved {
                        device: WaylandInputDevice::Keyboard(keyboard),
                    });
                }
            }
            seat::Capability::Pointer => {
                if let Some(relative_pointer) = capabilities.relative_pointer.take() {
                    relative_pointer.destroy();
                }
                if let Some(pointer) = capabilities.pointer.take() {
                    pointer.release();
                    self.send_input_event(InputEvent::DeviceRemoved {
                        device: WaylandInputDevice::Pointer(pointer),
                    });
                }
            }
            seat::Capability::Touch => {
                if let Some(touch) = capabilities.touch.take() {
                    touch.release();
                    self.send_input_event(InputEvent::DeviceRemoved {
                        device: WaylandInputDevice::Touch(touch),
                    });
                }
            }
            _ => warn!("Unknown capability {capability}"),
        }
    }

    fn remove_seat(&mut self, _: &Connection, _: &QueueHandle<Self>, seat: WlSeat) {
        self.seats.remove(&seat);
    }
}

smithay_client_toolkit::delegate_seat!(WaylandBackend);

impl KeyboardHandler for WaylandBackend {
    fn enter(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        keyboard: &WlKeyboard,
        surface: &WlSurface,
        serial: u32,
        raw: &[u32],
        _: &[Keysym],
    ) {
        assert_eq!(surface, self.graphics.window().wl_surface());
        self.send_input_event(InputEvent::Special(
            WaylandInputSpecialEvent::KeyboardEnter {
                keyboard: keyboard.clone(),
                serial,
                keys: raw.iter().copied().collect(),
            },
        ));
    }

    fn leave(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        keyboard: &WlKeyboard,
        surface: &WlSurface,
        serial: u32,
    ) {
        assert_eq!(surface, self.graphics.window().wl_surface());
        self.send_input_event(InputEvent::Special(
            WaylandInputSpecialEvent::KeyboardLeave {
                keyboard: keyboard.clone(),
                serial,
            },
        ));
    }

    fn press_key(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        keyboard: &WlKeyboard,
        serial: u32,
        event: seat::keyboard::KeyEvent,
    ) {
        self.send_input_event(InputEvent::Keyboard {
            event: WaylandKeyboardEvent {
                keyboard: keyboard.clone(),
                serial,
                time: event.time,
                key: event.raw_code,
                state: KeyState::Pressed,
            },
        });
    }

    fn release_key(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        keyboard: &WlKeyboard,
        serial: u32,
        event: seat::keyboard::KeyEvent,
    ) {
        self.send_input_event(InputEvent::Keyboard {
            event: WaylandKeyboardEvent {
                keyboard: keyboard.clone(),
                serial,
                time: event.time,
                key: event.raw_code,
                state: KeyState::Released,
            },
        });
    }

    fn update_modifiers(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        keyboard: &WlKeyboard,
        serial: u32,
        modifiers: Modifiers,
        _: u32,
    ) {
        self.send_input_event(InputEvent::Special(
            WaylandInputSpecialEvent::KeyboardModifiers {
                keyboard: keyboard.clone(),
                serial,
                modifiers,
            },
        ));
    }
}

smithay_client_toolkit::delegate_keyboard!(WaylandBackend);

impl PointerHandler for WaylandBackend {
    fn pointer_frame(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        pointer: &WlPointer,
        events: &[seat::pointer::PointerEvent],
    ) {
        for event in events {
            assert_eq!(&event.surface, self.graphics.window().wl_surface());
            let pointer = pointer.clone();

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

            let event = match event.kind {
                PointerEventKind::Enter { serial } => {
                    InputEvent::Special(WaylandInputSpecialEvent::PointerEnter { pointer, serial })
                }
                PointerEventKind::Leave { serial } => {
                    InputEvent::Special(WaylandInputSpecialEvent::PointerLeave { pointer, serial })
                }
                PointerEventKind::Motion { time } => InputEvent::PointerMotionAbsolute {
                    event: WaylandPointerMotionEvent {
                        pointer,
                        time,
                        x: event.position.0,
                        y: event.position.1,
                        window_size: self.graphics.window_size(),
                    },
                },
                PointerEventKind::Press {
                    serial,
                    time,
                    button,
                } => InputEvent::PointerButton {
                    event: WaylandPointerButtonEvent {
                        pointer,
                        serial,
                        time,
                        button,
                        state: ButtonState::Pressed,
                    },
                },
                PointerEventKind::Release {
                    serial,
                    time,
                    button,
                } => InputEvent::PointerButton {
                    event: WaylandPointerButtonEvent {
                        pointer,
                        serial,
                        time,
                        button,
                        state: ButtonState::Released,
                    },
                },
                PointerEventKind::Axis {
                    time,
                    horizontal,
                    vertical,
                    source,
                } => {
                    // input::AxisSource is exhaustive, so the "_" case is uninhabited in that type.
                    let source = source.map(|source| match source {
                        wl_pointer::AxisSource::Wheel => input::AxisSource::Wheel,
                        wl_pointer::AxisSource::Finger => input::AxisSource::Finger,
                        wl_pointer::AxisSource::Continuous => input::AxisSource::Continuous,
                        wl_pointer::AxisSource::WheelTilt => input::AxisSource::WheelTilt,
                        _ => unreachable!(),
                    });
                    InputEvent::PointerAxis {
                        event: WaylandPointerAxisEvent {
                            pointer,
                            time,
                            horizontal,
                            vertical,
                            source,
                        },
                    }
                }
            };

            self.send_input_event(event);
        }
    }
}

smithay_client_toolkit::delegate_pointer!(WaylandBackend);

impl RelativePointerHandler for WaylandBackend {
    fn relative_pointer_motion(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &ZwpRelativePointerV1,
        pointer: &wl_pointer::WlPointer,
        event: seat::relative_pointer::RelativeMotionEvent,
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

impl TouchHandler for WaylandBackend {
    fn down(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        touch: &WlTouch,
        serial: u32,
        time: u32,
        surface: WlSurface,
        id: i32,
        position: (f64, f64),
    ) {
        assert_eq!(&surface, self.graphics.window().wl_surface());
        self.send_input_event(InputEvent::TouchDown {
            event: WaylandTouchDownEvent {
                touch: touch.clone(),
                time,
                slot: Some(id as u32).into(),
                x: position.0,
                y: position.1,
                window_size: self.graphics.window_size(),
                serial,
            },
        });
    }

    fn up(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        touch: &WlTouch,
        serial: u32,
        time: u32,
        id: i32,
    ) {
        self.send_input_event(InputEvent::TouchUp {
            event: WaylandTouchUpEvent {
                touch: touch.clone(),
                time,
                slot: Some(id as u32).into(),
                serial,
            },
        });
    }

    fn motion(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        touch: &WlTouch,
        time: u32,
        id: i32,
        position: (f64, f64),
    ) {
        self.send_input_event(InputEvent::TouchMotion {
            event: WaylandTouchMotionEvent {
                touch: touch.clone(),
                time,
                slot: Some(id as u32).into(),
                x: position.0,
                y: position.1,
                window_size: self.graphics.window_size(),
            },
        });
    }

    fn shape(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &WlTouch,
        _: i32,
        _: f64,
        _: f64,
    ) {
        warn!("WlTouch::shape not implemented in Smithay; discarding this event");
    }

    fn orientation(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &WlTouch, _: i32, _: f64) {
        warn!("WlTouch::orientation not implemented in Smithay; discarding this event");
    }

    fn cancel(&mut self, _: &Connection, _: &QueueHandle<Self>, touch: &WlTouch) {
        self.send_input_event(InputEvent::TouchCancel {
            event: WaylandTouchCancelEvent {
                touch: touch.clone(),
                time: 0,
                slot: None.into(),
            },
        });
    }
}

smithay_client_toolkit::delegate_touch!(WaylandBackend);
