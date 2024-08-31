use std::collections::{HashMap, HashSet};
use std::fs::File;
use std::hash::{Hash, Hasher};
use std::io::Read;
use std::os::fd::OwnedFd;
use std::sync::{Mutex, MutexGuard};

use input::event::keyboard::KeyState;
use smithay::backend::input::{Device, DeviceCapability, InputBackend, UnusedEvent};
use smithay::input::keyboard::{xkb, KeymapFile, ModifiersState};
use smithay::input::{Seat, SeatHandler};
use smithay::output::Output;
use smithay::reexports::wayland_protocols_misc::zwp_virtual_keyboard_v1::server::zwp_virtual_keyboard_v1;
use smithay::reexports::wayland_protocols_misc::zwp_virtual_keyboard_v1::server::zwp_virtual_keyboard_manager_v1::{self, ZwpVirtualKeyboardManagerV1};
use smithay::reexports::wayland_protocols_misc::zwp_virtual_keyboard_v1::server::zwp_virtual_keyboard_v1::ZwpVirtualKeyboardV1;
use smithay::reexports::wayland_server::backend::ClientId;
use smithay::reexports::wayland_server::protocol::wl_keyboard::KeymapFormat;
use smithay::reexports::wayland_server::protocol::wl_seat::WlSeat;
use smithay::reexports::wayland_server::{
    Client, DataInit, Dispatch, DisplayHandle, GlobalDispatch, New, Resource,
};

const VERSION: u32 = 1;

pub struct VirtualKeyboardManagerState {
    virtual_keyboards: HashSet<ZwpVirtualKeyboardV1>,
}

pub struct VirtualKeyboardManagerGlobalData {
    filter: Box<dyn for<'c> Fn(&'c Client) -> bool + Send + Sync>,
}
pub struct VirtualKeyboardInputBackend;

#[derive(Clone, Debug, Hash, Eq, PartialEq)]
pub struct VirtualKeyboard {
    keyboard: ZwpVirtualKeyboardV1,
}

pub struct VirtualKeyboardUserData {
    seat: WlSeat,

    state: Mutex<Option<VirtualKeyboardState>>,
}

unsafe impl Send for VirtualKeyboardState {}

struct VirtualKeyboardState {
    keymap: KeymapFile,
    mods: ModifiersState,
    state: xkb::State,
}

impl VirtualKeyboard {
    fn data(&self) -> &VirtualKeyboardUserData {
        self.keyboard.data().unwrap()
    }

    fn seat(&self) -> &WlSeat {
        &self.data().seat
    }

    fn state(&self) -> MutexGuard<Option<VirtualKeyboardState>> {
        self.data().state.lock().unwrap()
    }
}

impl Device for VirtualKeyboard {
    fn id(&self) -> String {
        format!("wlr virtual keyboard {}", self.keyboard.id())
    }

    fn name(&self) -> String {
        String::from("virtual keyboard")
    }

    fn has_capability(&self, capability: DeviceCapability) -> bool {
        matches!(capability, DeviceCapability::Keyboard)
    }

    fn usb_id(&self) -> Option<(u32, u32)> {
        None
    }

    fn syspath(&self) -> Option<std::path::PathBuf> {
        None
    }
}

impl InputBackend for VirtualKeyboardInputBackend {
    type Device = VirtualKeyboard;

    type KeyboardKeyEvent = UnusedEvent;
    type PointerAxisEvent = UnusedEvent;
    type PointerButtonEvent = UnusedEvent;
    type PointerMotionEvent = UnusedEvent;
    type PointerMotionAbsoluteEvent = UnusedEvent;

    type GestureSwipeBeginEvent = UnusedEvent;
    type GestureSwipeUpdateEvent = UnusedEvent;
    type GestureSwipeEndEvent = UnusedEvent;
    type GesturePinchBeginEvent = UnusedEvent;
    type GesturePinchUpdateEvent = UnusedEvent;
    type GesturePinchEndEvent = UnusedEvent;
    type GestureHoldBeginEvent = UnusedEvent;
    type GestureHoldEndEvent = UnusedEvent;

    type TouchDownEvent = UnusedEvent;
    type TouchUpEvent = UnusedEvent;
    type TouchMotionEvent = UnusedEvent;
    type TouchCancelEvent = UnusedEvent;
    type TouchFrameEvent = UnusedEvent;
    type TabletToolAxisEvent = UnusedEvent;
    type TabletToolProximityEvent = UnusedEvent;
    type TabletToolTipEvent = UnusedEvent;
    type TabletToolButtonEvent = UnusedEvent;

    type SwitchToggleEvent = UnusedEvent;

    type SpecialEvent = UnusedEvent;
}

pub trait VirtualKeyboardHandler {
    fn virtual_keyboard_manager_state(&mut self) -> &mut VirtualKeyboardManagerState;

    fn create_virtual_keyboard(&mut self, keyboard: VirtualKeyboard) {
        let _ = keyboard;
    }
    fn destroy_virtual_keyboard(&mut self, keyboard: VirtualKeyboard) {
        let _ = keyboard;
    }
}

impl VirtualKeyboardManagerState {
    pub fn new<D, F>(display: &DisplayHandle, filter: F) -> Self
    where
        D: GlobalDispatch<ZwpVirtualKeyboardManagerV1, VirtualKeyboardManagerGlobalData>,
        D: Dispatch<ZwpVirtualKeyboardManagerV1, ()>,
        D: Dispatch<ZwpVirtualKeyboardV1, VirtualKeyboardUserData>,
        D: VirtualKeyboardHandler,
        D: 'static,
        F: for<'c> Fn(&'c Client) -> bool + Send + Sync + 'static,
    {
        let global_data = VirtualKeyboardManagerGlobalData {
            filter: Box::new(filter),
        };
        display.create_global::<D, ZwpVirtualKeyboardManagerV1, _>(VERSION, global_data);

        Self {
            virtual_keyboards: HashSet::new(),
        }
    }
}

impl<D> GlobalDispatch<ZwpVirtualKeyboardManagerV1, VirtualKeyboardManagerGlobalData, D>
    for VirtualKeyboardManagerState
where
    D: GlobalDispatch<ZwpVirtualKeyboardManagerV1, VirtualKeyboardManagerGlobalData>,
    D: Dispatch<ZwpVirtualKeyboardManagerV1, ()>,
    D: Dispatch<ZwpVirtualKeyboardV1, VirtualKeyboardUserData>,
    D: VirtualKeyboardHandler,
    D: 'static,
{
    fn bind(
        _state: &mut D,
        _handle: &DisplayHandle,
        _client: &Client,
        manager: New<ZwpVirtualKeyboardManagerV1>,
        _manager_state: &VirtualKeyboardManagerGlobalData,
        data_init: &mut DataInit<'_, D>,
    ) {
        data_init.init(manager, ());
    }

    fn can_view(client: Client, global_data: &VirtualKeyboardManagerGlobalData) -> bool {
        (global_data.filter)(&client)
    }
}

impl<D> Dispatch<ZwpVirtualKeyboardManagerV1, (), D> for VirtualKeyboardManagerState
where
    D: Dispatch<ZwpVirtualKeyboardManagerV1, ()>,
    D: Dispatch<ZwpVirtualKeyboardV1, VirtualKeyboardUserData>,
    D: VirtualKeyboardHandler,
    D: 'static,
{
    fn request(
        state: &mut D,
        _client: &Client,
        _resource: &ZwpVirtualKeyboardManagerV1,
        request: <ZwpVirtualKeyboardManagerV1 as Resource>::Request,
        _data: &(),
        _dhandle: &DisplayHandle,
        data_init: &mut DataInit<'_, D>,
    ) {
        match request {
            zwp_virtual_keyboard_manager_v1::Request::CreateVirtualKeyboard { id, seat } => {
                let keyboard = data_init.init(
                    id,
                    VirtualKeyboardUserData {
                        seat,
                        state: Mutex::new(None),
                    },
                );
                state
                    .virtual_keyboard_manager_state()
                    .virtual_keyboards
                    .insert(keyboard.clone());

                state.create_virtual_keyboard(VirtualKeyboard { keyboard });
            }
            _ => unreachable!(),
        }
    }
}

impl<D> Dispatch<ZwpVirtualKeyboardV1, VirtualKeyboardUserData, D> for VirtualKeyboardManagerState
where
    D: Dispatch<ZwpVirtualKeyboardV1, VirtualKeyboardUserData>,
    D: VirtualKeyboardHandler,
    D: SeatHandler + 'static,
    D: 'static,
{
    fn request(
        state: &mut D,
        _client: &Client,
        keyboard: &ZwpVirtualKeyboardV1,
        request: <ZwpVirtualKeyboardV1 as Resource>::Request,
        data: &VirtualKeyboardUserData,
        _dhandle: &DisplayHandle,
        _data_init: &mut DataInit<'_, D>,
    ) {
        let keyboard = VirtualKeyboard {
            keyboard: keyboard.clone(),
        };

        match request {
            zwp_virtual_keyboard_v1::Request::Keymap { format, fd, size } => {
                // Only libxkbcommon compatible keymaps are supported.
                if format != KeymapFormat::XkbV1 as u32 {
                    debug!("Unsupported keymap format: {format:?}");
                    return;
                }

                let context = xkb::Context::new(xkb::CONTEXT_NO_FLAGS);
                // SAFETY: we can map the keymap into the memory.
                let new_keymap = match unsafe {
                    xkb::Keymap::new_from_fd(
                        &context,
                        fd,
                        size as usize,
                        xkb::KEYMAP_FORMAT_TEXT_V1,
                        xkb::KEYMAP_COMPILE_NO_FLAGS,
                    )
                } {
                    Ok(Some(new_keymap)) => new_keymap,
                    Ok(None) => {
                        debug!("Invalid libxkbcommon keymap");
                        return;
                    }
                    Err(err) => {
                        debug!("Could not map the keymap: {err:?}");
                        return;
                    }
                };

                let mut state = keyboard.state();
                *state = Some(VirtualKeyboardState {
                    mods: state.take().map(|state| state.mods).unwrap_or_default(),
                    keymap: KeymapFile::new(&new_keymap),
                    state: xkb::State::new(&new_keymap),
                });
            }
            zwp_virtual_keyboard_v1::Request::Key { time, key, state } => {
                // Ensure keymap was initialized.
                let state = keyboard.state();
                let vk_state = match state.as_mut() {
                    Some(vk_state) => vk_state,
                    None => {
                        keyboard.keyboard.post_error(zwp_virtual_keyboard_v1::Error::NoKeymap, "`key` sent before keymap.");
                        return;
                    }
                };

                let seat = Seat::<D>::from_resource(keyboard.seat()).unwrap();
            }
            zwp_virtual_keyboard_v1::Request::Modifiers {
                mods_depressed,
                mods_latched,
                mods_locked,
                group,
            } => {}
            zwp_virtual_keyboard_v1::Request::Destroy => (),
            _ => unreachable!(),
        }
    }
}

#[macro_export]
macro_rules! delegate_virtual_keyboard {
    ($(@<$( $lt:tt $( : $clt:tt $(+ $dlt:tt )* )? ),+>)? $ty: ty) => {
        smithay::reexports::wayland_server::delegate_global_dispatch!($(@< $( $lt $( : $clt $(+ $dlt )* )? ),+ >)? $ty: [
            smithay::reexports::wayland_protocols_misc::zwp_virtual_keyboard_v1::server::zwp_virtual_keyboard_manager_v1::ZwpVirtualKeyboardManagerV1: $crate::protocols::virtual_keyboard::VirtualKeyboardManagerGlobalData
        ] => $crate::protocols::virtual_keyboard::VirtualKeyboardManagerState);

        smithay::reexports::wayland_server::delegate_dispatch!($(@< $( $lt $( : $clt $(+ $dlt )* )? ),+ >)? $ty: [
            smithay::reexports::wayland_protocols_misc::zwp_virtual_keyboard_v1::server::zwp_virtual_keyboard_manager_v1::ZwpVirtualKeyboardManagerV1: ()
        ] => $crate::protocols::virtual_keyboard::VirtualKeyboardManagerState);

        smithay::reexports::wayland_server::delegate_dispatch!($(@< $( $lt $( : $clt $(+ $dlt )* )? ),+ >)? $ty: [
            smithay::reexports::wayland_protocols_misc::zwp_virtual_keyboard_v1::server::zwp_virtual_keyboard_v1::ZwpVirtualKeyboardV1:  $crate::protocols::virtual_keyboard::VirtualKeyboardUserData
        ] => $crate::protocols::virtual_keyboard::VirtualKeyboardManagerState);
    };
}
