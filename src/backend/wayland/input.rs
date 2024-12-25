use std::collections::HashSet;
use std::os::fd::OwnedFd;

use smithay::backend::input::{
    Axis, AxisRelativeDirection, AxisSource, ButtonState, Device, DeviceCapability, InputBackend,
    KeyState, Keycode, TouchSlot, UnusedEvent,
};
use smithay::utils::{Logical, Physical, Point, Size, Transform};
use smithay_client_toolkit::reexports::client::protocol::wl_keyboard::WlKeyboard;
use smithay_client_toolkit::reexports::client::protocol::wl_pointer::WlPointer;
use smithay_client_toolkit::reexports::client::protocol::wl_touch::WlTouch;
use smithay_client_toolkit::seat;

#[derive(Debug)]
pub struct WaylandInputBackend;

#[derive(Clone, Debug, Hash, Eq, PartialEq)]
pub enum WaylandInputDevice {
    Keyboard(WlKeyboard),
    Pointer(WlPointer),
    Touch(WlTouch),
}

impl From<WlKeyboard> for WaylandInputDevice {
    fn from(keyboard: WlKeyboard) -> Self {
        WaylandInputDevice::Keyboard(keyboard)
    }
}

impl From<WlPointer> for WaylandInputDevice {
    fn from(pointer: WlPointer) -> Self {
        WaylandInputDevice::Pointer(pointer)
    }
}

impl From<WlTouch> for WaylandInputDevice {
    fn from(touch: WlTouch) -> Self {
        WaylandInputDevice::Touch(touch)
    }
}

impl Device for WaylandInputDevice {
    fn id(&self) -> String {
        match self {
            WaylandInputDevice::Keyboard(keyboard) => format!("{keyboard:?}"),
            WaylandInputDevice::Pointer(pointer) => format!("{pointer:?}"),
            WaylandInputDevice::Touch(touch) => format!("{touch:?}"),
        }
    }

    fn name(&self) -> String {
        match self {
            WaylandInputDevice::Keyboard(_) => "WlKeyboard".to_string(),
            WaylandInputDevice::Pointer(_) => "WlPointer".to_string(),
            WaylandInputDevice::Touch(_) => "WlTouch".to_string(),
        }
    }

    fn has_capability(&self, capability: DeviceCapability) -> bool {
        (match self {
            WaylandInputDevice::Keyboard(_) => DeviceCapability::Keyboard,
            WaylandInputDevice::Pointer(_) => DeviceCapability::Pointer,
            WaylandInputDevice::Touch(_) => DeviceCapability::Touch,
        }) == capability
    }

    fn usb_id(&self) -> Option<(u32, u32)> {
        None
    }

    fn syspath(&self) -> Option<std::path::PathBuf> {
        None
    }
}

#[derive(Copy, Clone)]
pub struct RawAbsolutePosition {
    pos: Point<f64, Physical>,
    transform: Transform,
    window_size: Size<i32, Physical>,
}

impl RawAbsolutePosition {
    pub fn new(
        surface_x: f64,
        surface_y: f64,
        transform: Transform,
        window_size: Size<i32, Physical>,
    ) -> Self {
        RawAbsolutePosition {
            pos: (surface_x, surface_y).into(),
            transform,
            window_size,
        }
    }

    pub fn physical_bounds(mut self) -> Size<f64, Physical> {
        self.window_size -= (1, 1).into();
        self.window_size.to_f64()
    }

    pub fn logical_bounds(self) -> Size<f64, Logical> {
        self.transform
            .transform_size(self.window_size.to_f64())
            .to_logical(1.0)
    }

    pub fn position(self) -> Point<f64, Logical> {
        self.transform
            .transform_point_in(self.pos, &self.physical_bounds())
            .to_logical(1.0)
    }

    pub fn x(self) -> f64 {
        self.position().x
    }

    pub fn y(self) -> f64 {
        self.position().y
    }

    pub fn x_transformed(self, width: i32) -> f64 {
        self.x() / self.logical_bounds().w * width as f64
    }

    pub fn y_transformed(self, height: i32) -> f64 {
        self.y() / self.logical_bounds().h * height as f64
    }
}

macro_rules! event {
    (
        pub struct $event:ident {
            pub $device:ident: $device_ty:ty,
            $(pub $field:ident: $field_ty:ty,)*
            $(#[TouchEvent] {
                    pub $touch_slot:ident: TouchSlot,
            })?
            $(#[AbsolutePositionEvent] {
                pub $x:ident: f64,
                pub $y:ident: f64,
                pub $transform:ident: Transform,
                pub $window_size:ident: Size<i32, Physical>,
            })?
            $(
                fn time($($param:tt)*) -> $ret:ty $time:block
            )?
        }

        $(impl $trait:ident {
            $($impl:tt)*
        })*
    ) => {
        #[derive(Debug)]
        pub struct $event {
            pub $device: $device_ty,
            $(pub $field: $field_ty,)*
            $(
                pub $touch_slot: TouchSlot,
            )?
            $(
                pub $x: f64,
                pub $y: f64,
                pub $transform: Transform,
                pub $window_size: Size<i32, Physical>,
            )?
        }

        $(
            impl $event {
                fn __time($($param)*) -> $ret $time
            }
        )?

        impl ::smithay::backend::input::Event<WaylandInputBackend> for $event {
            fn time(&self) -> u64 {
                $(
                    let v: $ret = self.__time();
                    return v;
                    #[cfg(any())]
                )?
                {
                    self.time as u64 * 1000 // millis to micros
                }
            }

            fn device(&self) -> WaylandInputDevice {
                self.$device.clone().into()
            }
        }

        $(
            impl ::smithay::backend::input::TouchEvent<WaylandInputBackend> for $event {
                fn slot(&self) -> TouchSlot {
                    self.$touch_slot
                }
            }
        )?

        $(
            impl ::smithay::backend::input::AbsolutePositionEvent<WaylandInputBackend> for $event
{               fn x(&self) -> f64 {
                    RawAbsolutePosition::new(self.$x, self.$y, self.$transform, self.$window_size).x()
                }

                fn y(&self) -> f64 {
                    RawAbsolutePosition::new(self.$x, self.$y, self.$transform, self.$window_size).y()
                }

                fn x_transformed(&self, width: i32) -> f64 {
                    RawAbsolutePosition::new(self.$x, self.$y, self.$transform, self.$window_size).x_transformed(width)
                }

                fn y_transformed(&self, height: i32) -> f64 {
                    RawAbsolutePosition::new(self.$x, self.$y, self.$transform, self.$window_size).y_transformed(height)
                }
            }
        )?

        $(
            impl ::smithay::backend::input::$trait<WaylandInputBackend> for $event {
                $($impl)*
            }
        )*
    };
}

event!(
    pub struct WaylandKeyboardEvent {
        pub keyboard: WlKeyboard,
        pub time: u32,
        pub serial: u32,
        pub key: Keycode,
        pub state: KeyState,
    }

    impl KeyboardKeyEvent {
        fn key_code(&self) -> Keycode {
            self.key
        }

        fn state(&self) -> KeyState {
            self.state
        }

        fn count(&self) -> u32 {
            match self.state {
                KeyState::Pressed => 1,
                KeyState::Released => 0,
            }
        }
    }
);

fn transform_delta(transform: Transform, (dx, dy): (f64, f64)) -> (f64, f64) {
    let transform = match transform {
        Transform::Flipped90 => Transform::Flipped270,
        Transform::Flipped270 => Transform::Flipped90,
        other => other,
    };
    match transform {
        Transform::Normal => (dx, dy),
        Transform::_90 => (-dy, dx),
        Transform::_180 => (-dx, -dy),
        Transform::_270 => (dy, -dx),
        Transform::Flipped => (-dx, dy),
        Transform::Flipped90 => (-dy, -dx),
        Transform::Flipped180 => (dx, -dy),
        Transform::Flipped270 => (dy, dx),
    }
}

event!(
    pub struct WaylandPointerRelativeMotionEvent {
        pub pointer: WlPointer,
        pub relative_motion: seat::relative_pointer::RelativeMotionEvent,
        pub transform: Transform,
        fn time(&self) -> u64 {
            self.relative_motion.utime // this one is already in micros! :3
        }
    }

    impl PointerMotionEvent
    {
        fn delta_x(&self) -> f64 {
            transform_delta(self.transform, self.relative_motion.delta).0
        }

        fn delta_y(&self) -> f64 {
            transform_delta(self.transform, self.relative_motion.delta).1
        }

        fn delta_x_unaccel(&self) -> f64 {
            transform_delta(self.transform, self.relative_motion.delta_unaccel).0
        }

        fn delta_y_unaccel(&self) -> f64 {
            transform_delta(self.transform, self.relative_motion.delta_unaccel).0
        }
    }
);

event!(
    pub struct WaylandPointerMotionEvent {
        pub pointer: WlPointer,
        pub time: u32,
        #[AbsolutePositionEvent] {
            pub x: f64,
            pub y: f64,
            pub transform: Transform,
            pub window_size: Size<i32, Physical>,
        }
    }
    impl PointerMotionAbsoluteEvent {}
);

event!(
    pub struct WaylandPointerButtonEvent {
        pub pointer: WlPointer,
        pub time: u32,
        pub serial: u32,
        pub button: u32,
        pub state: ButtonState,
    }
    impl PointerButtonEvent {
        fn button_code(&self) -> u32 {
            self.button
        }

        fn state(&self) -> ButtonState {
            self.state
        }
    }
);

#[derive(Debug, PartialEq, Clone)]
pub struct AxisFrame {
    pub time: u32,
    pub horizontal: AxisScroll,
    pub vertical: AxisScroll,
    pub source: AxisSource,
}

#[derive(Debug, PartialEq, Clone)]
pub struct AxisScroll {
    pub absolute: f64,
    pub v120: i32,
    pub relative_direction: AxisRelativeDirection,
}

impl std::ops::Index<Axis> for AxisFrame {
    type Output = AxisScroll;

    fn index(&self, axis: Axis) -> &Self::Output {
        match axis {
            Axis::Vertical => &self.vertical,
            Axis::Horizontal => &self.horizontal,
        }
    }
}

impl std::ops::IndexMut<Axis> for AxisFrame {
    fn index_mut(&mut self, axis: Axis) -> &mut Self::Output {
        match axis {
            Axis::Vertical => &mut self.vertical,
            Axis::Horizontal => &mut self.horizontal,
        }
    }
}

impl Default for AxisFrame {
    fn default() -> Self {
        AxisFrame {
            time: 0, // Should always be overwritten.
            horizontal: AxisScroll {
                absolute: 0.0,
                v120: 0,
                relative_direction: AxisRelativeDirection::Identical,
            },
            vertical: AxisScroll {
                absolute: 0.0,
                v120: 0,
                relative_direction: AxisRelativeDirection::Identical,
            },
            // I assume most compositors always send an axis source (we certainly do in niri).
            // As such, this "should" always be overwritten. If it isn't, it's probably a bug,
            // But maybe the compositor doesn't support v5 of the wl_pointer protocol at all.
            // In that case, we know we won't get any axis_source, and i think for such an old
            // compositor we're most likely to be dealing with a Wheel.
            source: AxisSource::Wheel,
        }
    }
}

impl AxisFrame {
    pub fn time(&mut self, time: u32) {
        if self.time == 0 {
            self.time = time;
        }
    }
}

event!(
    pub struct WaylandPointerAxisEvent {
        pub pointer: WlPointer,
        pub axis_frame: AxisFrame,
        fn time(&self) -> u64 {
            self.axis_frame.time as u64 * 1000 // millis to micros
        }
    }

    impl PointerAxisEvent {
        fn amount(&self, axis: Axis) -> Option<f64> {
            Some(self.axis_frame[axis].absolute)
        }

        fn amount_v120(&self, axis: Axis) -> Option<f64> {
            Some(self.axis_frame[axis].v120 as f64)
        }

        fn source(&self) -> AxisSource {
            self.axis_frame.source
        }

        fn relative_direction(&self, axis: Axis) -> AxisRelativeDirection {
            self.axis_frame[axis].relative_direction
        }
    }
);

event!(
    pub struct WaylandTouchDownEvent {
        pub touch: WlTouch,
        pub time: u32,
        pub serial: u32,
        #[TouchEvent] {
            pub slot: TouchSlot,
        }
        #[AbsolutePositionEvent] {
            pub x: f64,
            pub y: f64,
            pub transform: Transform,
            pub window_size: Size<i32, Physical>,
        }
    }
    impl TouchDownEvent {}
);

event!(
    pub struct WaylandTouchUpEvent {
        pub touch: WlTouch,
        pub time: u32,
        pub serial: u32,
        #[TouchEvent] {
            pub slot: TouchSlot,
        }
    }
    impl TouchUpEvent {}
);

event!(
    pub struct WaylandTouchMotionEvent {
        pub touch: WlTouch,
        pub time: u32,
        #[TouchEvent] {
            pub slot: TouchSlot,
        }
        #[AbsolutePositionEvent] {
            pub x: f64,
            pub y: f64,
            pub transform: Transform,
            pub window_size: Size<i32, Physical>,
        }
    }
    impl TouchMotionEvent {}
);

event!(
    pub struct WaylandTouchCancelEvent {
        pub touch: WlTouch,
        pub time: u32,
        #[TouchEvent] {
            pub slot: TouchSlot,
        }
    }
    impl TouchCancelEvent {}
);

event!(
    pub struct WaylandTouchFrameEvent {
        pub touch: WlTouch,
        pub time: u32,
    }
    impl TouchFrameEvent {}
);

#[derive(Debug)]
pub enum WaylandInputSpecialEvent {
    PointerEnter {
        pointer: WlPointer,
        serial: u32,
        surface_x: f64,
        surface_y: f64,
        transform: Transform,
        window_size: Size<i32, Physical>,
    },
    PointerLeave {
        pointer: WlPointer,
        serial: u32,
    },
    KeyboardEnter {
        keyboard: WlKeyboard,
        serial: u32,
        keys: HashSet<Keycode>,
    },
    KeyboardLeave {
        keyboard: WlKeyboard,
        serial: u32,
    },
    KeyboardKeymap {
        keyboard: WlKeyboard,
        fd: OwnedFd,
        size: u32,
    },
    KeyboardModifiers {
        keyboard: WlKeyboard,
        serial: u32,
        depressed: u32,
        latched: u32,
        locked: u32,
        group: u32,
    },
    KeyboardRepeatInfo {
        keyboard: WlKeyboard,
        rate: i32,
        delay: i32,
    },
}

impl InputBackend for WaylandInputBackend {
    type Device = WaylandInputDevice;

    type KeyboardKeyEvent = WaylandKeyboardEvent;
    type PointerAxisEvent = WaylandPointerAxisEvent;
    type PointerButtonEvent = WaylandPointerButtonEvent;
    type PointerMotionEvent = WaylandPointerRelativeMotionEvent;
    type PointerMotionAbsoluteEvent = WaylandPointerMotionEvent;

    type GestureSwipeBeginEvent = UnusedEvent;
    type GestureSwipeUpdateEvent = UnusedEvent;
    type GestureSwipeEndEvent = UnusedEvent;
    type GesturePinchBeginEvent = UnusedEvent;
    type GesturePinchUpdateEvent = UnusedEvent;
    type GesturePinchEndEvent = UnusedEvent;
    type GestureHoldBeginEvent = UnusedEvent;
    type GestureHoldEndEvent = UnusedEvent;

    type TouchDownEvent = WaylandTouchDownEvent;
    type TouchUpEvent = WaylandTouchUpEvent;
    type TouchMotionEvent = WaylandTouchMotionEvent;
    type TouchCancelEvent = WaylandTouchCancelEvent;
    type TouchFrameEvent = WaylandTouchFrameEvent;
    type TabletToolAxisEvent = UnusedEvent;
    type TabletToolProximityEvent = UnusedEvent;
    type TabletToolTipEvent = UnusedEvent;
    type TabletToolButtonEvent = UnusedEvent;

    type SwitchToggleEvent = UnusedEvent;

    type SpecialEvent = WaylandInputSpecialEvent;
}
