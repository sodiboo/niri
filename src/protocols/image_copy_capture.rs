use std::collections::HashSet;

use smithay::{output::{Output, WeakOutput}, reexports::{wayland_protocols::ext::{image_capture_source::v1::server::{ext_foreign_toplevel_image_capture_source_manager_v1::{self, ExtForeignToplevelImageCaptureSourceManagerV1}, ext_image_capture_source_v1::{self, ExtImageCaptureSourceV1}, ext_output_image_capture_source_manager_v1::{self, ExtOutputImageCaptureSourceManagerV1}}, image_copy_capture::v1::server::{ext_image_copy_capture_cursor_session_v1::{self, ExtImageCopyCaptureCursorSessionV1}, ext_image_copy_capture_frame_v1::{self, ExtImageCopyCaptureFrameV1}, ext_image_copy_capture_manager_v1::{self, ExtImageCopyCaptureManagerV1}, ext_image_copy_capture_session_v1::{self, ExtImageCopyCaptureSessionV1}}}, wayland_server::{Client, DataInit, Dispatch, DisplayHandle, GlobalDispatch, New}}};

use crate::window::mapped::MappedId;

use super::foreign_toplevel::ForeignToplevelHandler;

const FOREIGN_TOPLEVEL_SOURCE_VERSION: u32 = 1;
const OUTPUT_SOURCE_VERSION: u32 = 1;
const IMAGE_COPY_CAPTURE_VERSION: u32 = 1;

pub enum ImageCaptureSource {
    Invalid,
    Toplevel(MappedId),
    Output(WeakOutput),
}

pub trait ImageCopyCaptureHandler {
    fn image_capture_state(&mut self) -> &mut ImageCopyCaptureState;
}

// Security note: These filters are applied to each way to construct an image capture source, such
// that in theory granular permissions can be applied and only allow a client to capture from
// certain sources, only outputs, only toplevels, etc. In practice, niri currently only has a
// boolean "restricted" condition under which all image capture sources are disallowed, so this
// isn't utilized at all really.
//
// The primary ext-image-copy-capture protocol is *not* filtered, and is always available to all
// clients. This is completely fine, as the copy capture protocol really only exposes a capability
// of the compositor to capture certain things; but without the image sources, there's no way to
// actually tell the compositor what to capture in the first place. If we want to add capture
// sources that are unrestricted in the future, we can do so by adding a new global, and sandboxed
// clients will only be able to capture from the restricted sources.
pub struct ImageCaptureSourceGlobalData {
    filter: Box<dyn for<'c> Fn(&'c Client) -> bool + Send + Sync>,
}

pub struct ImageCopyCaptureState {
    foreign_toplevel_source_managers: HashSet<ExtForeignToplevelImageCaptureSourceManagerV1>,
    output_source_managers: HashSet<ExtOutputImageCaptureSourceManagerV1>,
    image_copy_capture_managers: HashSet<ExtImageCopyCaptureManagerV1>,
}

impl ImageCopyCaptureState {
    pub fn new<D>(
        display: &DisplayHandle,
        foreign_toplevel_source_filter: impl for<'c> Fn(&'c Client) -> bool + Send + Sync + 'static,
        output_source_filter: impl for<'c> Fn(&'c Client) -> bool + Send + Sync + 'static,
    ) -> Self
    where
        D: ImageCopyCaptureHandler + 'static,
        D: GlobalDispatch<
            ExtForeignToplevelImageCaptureSourceManagerV1,
            ImageCaptureSourceGlobalData,
        >,
        D: GlobalDispatch<ExtOutputImageCaptureSourceManagerV1, ImageCaptureSourceGlobalData>,
        D: GlobalDispatch<ExtImageCopyCaptureManagerV1, ()>,
    {
        display.create_global::<D, ExtForeignToplevelImageCaptureSourceManagerV1, _>(
            FOREIGN_TOPLEVEL_SOURCE_VERSION,
            ImageCaptureSourceGlobalData {
                filter: Box::new(foreign_toplevel_source_filter),
            },
        );

        display.create_global::<D, ExtOutputImageCaptureSourceManagerV1, _>(
            OUTPUT_SOURCE_VERSION,
            ImageCaptureSourceGlobalData {
                filter: Box::new(output_source_filter),
            },
        );

        display.create_global::<D, ExtImageCopyCaptureManagerV1, _>(IMAGE_COPY_CAPTURE_VERSION, ());

        Self {
            foreign_toplevel_source_managers: HashSet::new(),
            output_source_managers: HashSet::new(),
            image_copy_capture_managers: HashSet::new(),
        }
    }
}

impl<D> Dispatch<ExtImageCaptureSourceV1, ImageCaptureSource, D> for ImageCopyCaptureState {
    fn request(
        _state: &mut D,
        _client: &Client,
        _resource: &ExtImageCaptureSourceV1,
        request: <ExtImageCaptureSourceV1 as smithay::reexports::wayland_server::Resource>::Request,
        _data: &ImageCaptureSource,
        _dhandle: &DisplayHandle,
        _data_init: &mut DataInit<'_, D>,
    ) {
        match request {
            ext_image_capture_source_v1::Request::Destroy => {
                // Nothing to do. Clean up ImageCaptureSource if we need to in the future.
            }
            _ => unreachable!(),
        }
    }
}

impl<D>
    GlobalDispatch<ExtForeignToplevelImageCaptureSourceManagerV1, ImageCaptureSourceGlobalData, D>
    for ImageCopyCaptureState
where
    D: ImageCopyCaptureHandler
        + Dispatch<ExtForeignToplevelImageCaptureSourceManagerV1, (), D>
        + ForeignToplevelHandler,
{
    fn bind(
        state: &mut D,
        _handle: &DisplayHandle,
        _client: &Client,
        resource: New<ExtForeignToplevelImageCaptureSourceManagerV1>,
        _global_data: &ImageCaptureSourceGlobalData,
        data_init: &mut DataInit<'_, D>,
    ) {
        let manager = data_init.init(resource, ());

        state
            .image_capture_state()
            .foreign_toplevel_source_managers
            .insert(manager);
    }

    fn can_view(client: Client, global_data: &ImageCaptureSourceGlobalData) -> bool {
        (global_data.filter)(&client)
    }
}

impl<D> Dispatch<ExtForeignToplevelImageCaptureSourceManagerV1, (), D> for ImageCopyCaptureState
where
    D: ImageCopyCaptureHandler,
    D: ForeignToplevelHandler,
    D: Dispatch<ExtForeignToplevelImageCaptureSourceManagerV1, (), D>,
    D: Dispatch<ExtImageCaptureSourceV1, ImageCaptureSource>,
{
    fn request(
        state: &mut D,
        _client: &Client,
        resource: &ExtForeignToplevelImageCaptureSourceManagerV1,
        request: <ExtForeignToplevelImageCaptureSourceManagerV1 as smithay::reexports::wayland_server::Resource>::Request,
        _data: &(),
        _dhandle: &DisplayHandle,
        data_init: &mut DataInit<'_, D>,
    ) {
        match request {
            ext_foreign_toplevel_image_capture_source_manager_v1::Request::CreateSource {
                source,
                toplevel_handle,
            } => {
                data_init.init(
                    source,
                    if let Some(id) = state
                        .foreign_toplevel_manager_state()
                        .get_identifier(&toplevel_handle)
                    {
                        ImageCaptureSource::Toplevel(id)
                    } else {
                        ImageCaptureSource::Invalid
                    },
                );
            }
            ext_foreign_toplevel_image_capture_source_manager_v1::Request::Destroy => {
                state
                    .image_capture_state()
                    .foreign_toplevel_source_managers
                    .remove(resource);
            }
            _ => unreachable!(),
        }
    }

    fn destroyed(
        _state: &mut D,
        _client: wayland_backend::server::ClientId,
        _resource: &ExtForeignToplevelImageCaptureSourceManagerV1,
        _data: &(),
    ) {
    }
}

impl<D> GlobalDispatch<ExtOutputImageCaptureSourceManagerV1, ImageCaptureSourceGlobalData, D>
    for ImageCopyCaptureState
where
    D: ImageCopyCaptureHandler + Dispatch<ExtOutputImageCaptureSourceManagerV1, (), D>,
{
    fn bind(
        state: &mut D,
        _handle: &DisplayHandle,
        _client: &Client,
        resource: New<ExtOutputImageCaptureSourceManagerV1>,
        _global_data: &ImageCaptureSourceGlobalData,
        data_init: &mut DataInit<'_, D>,
    ) {
        let manager = data_init.init(resource, ());

        state
            .image_capture_state()
            .output_source_managers
            .insert(manager);
    }

    fn can_view(client: Client, global_data: &ImageCaptureSourceGlobalData) -> bool {
        (global_data.filter)(&client)
    }
}

impl<D> Dispatch<ExtOutputImageCaptureSourceManagerV1, (), D> for ImageCopyCaptureState
where
    D: ImageCopyCaptureHandler,
    D: Dispatch<ExtOutputImageCaptureSourceManagerV1, (), D>,
    D: Dispatch<ExtImageCaptureSourceV1, ImageCaptureSource>,
{
    fn request(
        state: &mut D,
        _client: &Client,
        resource: &ExtOutputImageCaptureSourceManagerV1,
        request: <ExtOutputImageCaptureSourceManagerV1 as smithay::reexports::wayland_server::Resource>::Request,
        _data: &(),
        _dhandle: &DisplayHandle,
        data_init: &mut DataInit<'_, D>,
    ) {
        match request {
            ext_output_image_capture_source_manager_v1::Request::CreateSource {
                source,
                output,
            } => {
                data_init.init(
                    source,
                    if let Some(output) = Output::from_resource(&output) {
                        ImageCaptureSource::Output(output.downgrade())
                    } else {
                        ImageCaptureSource::Invalid
                    },
                );
            }
            ext_output_image_capture_source_manager_v1::Request::Destroy => {
                state
                    .image_capture_state()
                    .output_source_managers
                    .remove(resource);
            }
            _ => todo!(),
        }
    }

    fn destroyed(
        _state: &mut D,
        _client: wayland_backend::server::ClientId,
        _resource: &ExtOutputImageCaptureSourceManagerV1,
        _data: &(),
    ) {
    }
}

impl<D> GlobalDispatch<ExtImageCopyCaptureManagerV1, (), D> for ImageCopyCaptureState
where
    D: ImageCopyCaptureHandler,
    D: Dispatch<ExtImageCopyCaptureManagerV1, (), D>,
{
    fn bind(
        state: &mut D,
        _handle: &DisplayHandle,
        _client: &Client,
        resource: New<ExtImageCopyCaptureManagerV1>,
        _global_data: &(),
        data_init: &mut DataInit<'_, D>,
    ) {
        let manager = data_init.init(resource, ());

        state
            .image_capture_state()
            .image_copy_capture_managers
            .insert(manager);
    }
}

impl<D> Dispatch<ExtImageCopyCaptureManagerV1, (), D> for ImageCopyCaptureState
where
    D: ImageCopyCaptureHandler,
{
    fn request(
        state: &mut D,
        client: &Client,
        resource: &ExtImageCopyCaptureManagerV1,
        request: <ExtImageCopyCaptureManagerV1 as smithay::reexports::wayland_server::Resource>::Request,
        data: &(),
        dhandle: &DisplayHandle,
        data_init: &mut DataInit<'_, D>,
    ) {
        match request {
            ext_image_copy_capture_manager_v1::Request::CreateSession {
                session,
                source,
                options,
            } => {
                todo!();
            }
            ext_image_copy_capture_manager_v1::Request::CreatePointerCursorSession {
                session,
                source,
                pointer,
            } => {
                todo!();
            }
            ext_image_copy_capture_manager_v1::Request::Destroy => {
                todo!();
            }
            _ => unreachable!(),
        }
    }
}

pub struct ImageCopyCaptureSessionData;

impl<D> Dispatch<ExtImageCopyCaptureSessionV1, ImageCopyCaptureSessionData, D>
    for ImageCopyCaptureState
{
    fn request(
        state: &mut D,
        client: &Client,
        resource: &ExtImageCopyCaptureSessionV1,
        request: <ExtImageCopyCaptureSessionV1 as smithay::reexports::wayland_server::Resource>::Request,
        data: &ImageCopyCaptureSessionData,
        dhandle: &DisplayHandle,
        data_init: &mut DataInit<'_, D>,
    ) {
        match request {
            ext_image_copy_capture_session_v1::Request::CreateFrame { frame } => {
                todo!();
            }
            ext_image_copy_capture_session_v1::Request::Destroy => {
                todo!();
            }
            _ => unreachable!(),
        }
    }
}

pub struct ImageCopyCaptureFrameData;

impl<D> Dispatch<ExtImageCopyCaptureFrameV1, ImageCopyCaptureFrameData, D>
    for ImageCopyCaptureState
{
    fn request(
        state: &mut D,
        client: &Client,
        resource: &ExtImageCopyCaptureFrameV1,
        request: <ExtImageCopyCaptureFrameV1 as smithay::reexports::wayland_server::Resource>::Request,
        data: &ImageCopyCaptureFrameData,
        dhandle: &DisplayHandle,
        data_init: &mut DataInit<'_, D>,
    ) {
        match request {
            ext_image_copy_capture_frame_v1::Request::Destroy => {
                todo!();
            }
            ext_image_copy_capture_frame_v1::Request::AttachBuffer { buffer } => {
                todo!();
            }
            ext_image_copy_capture_frame_v1::Request::DamageBuffer {
                x,
                y,
                width,
                height,
            } => {
                todo!();
            }
            ext_image_copy_capture_frame_v1::Request::Capture => {
                todo!();
            }
            _ => unreachable!(),
        }
    }
}

pub struct ImageCopyCaptureCursorSessionData;

impl<D> Dispatch<ExtImageCopyCaptureCursorSessionV1, ImageCopyCaptureCursorSessionData, D>
    for ImageCopyCaptureState
{
    fn request(
        state: &mut D,
        client: &Client,
        resource: &ExtImageCopyCaptureCursorSessionV1,
        request: <ExtImageCopyCaptureCursorSessionV1 as smithay::reexports::wayland_server::Resource>::Request,
        data: &ImageCopyCaptureCursorSessionData,
        dhandle: &DisplayHandle,
        data_init: &mut DataInit<'_, D>,
    ) {
        match request {
            ext_image_copy_capture_cursor_session_v1::Request::Destroy => {
                todo!();
            }
            ext_image_copy_capture_cursor_session_v1::Request::GetCaptureSession { session } => {
                todo!();
            }
            _ => unreachable!(),
        }
    }
}

#[macro_export]
macro_rules! delegate_image_copy_capture {
    ($(@<$( $lt:tt $( : $clt:tt $(+ $dlt:tt )* )? ),+>)? $ty: ty) => {

        // ext_image_capture_source_v1

        smithay::reexports::wayland_server::delegate_dispatch!($(@< $( $lt $( : $clt $(+ $dlt )* )? ),+ >)? $ty: [
            smithay::reexports::wayland_protocols::ext::image_capture_source::v1::server::ext_image_capture_source_v1::ExtImageCaptureSourceV1: $crate::protocols::image_copy_capture::ImageCaptureSource
        ] => $crate::protocols::image_copy_capture::ImageCopyCaptureState);

        // ext_foreign_toplevel_image_capture_source_manager_v1

        smithay::reexports::wayland_server::delegate_global_dispatch!($(@< $( $lt $( : $clt $(+ $dlt )* )? ),+ >)? $ty: [
            smithay::reexports::wayland_protocols::ext::image_capture_source::v1::server::ext_foreign_toplevel_image_capture_source_manager_v1::ExtForeignToplevelImageCaptureSourceManagerV1: $crate::protocols::image_copy_capture::ImageCaptureSourceGlobalData
        ] => $crate::protocols::image_copy_capture::ImageCopyCaptureState);

        smithay::reexports::wayland_server::delegate_dispatch!($(@< $( $lt $( : $clt $(+ $dlt )* )? ),+ >)? $ty: [
            smithay::reexports::wayland_protocols::ext::image_capture_source::v1::server::ext_foreign_toplevel_image_capture_source_manager_v1::ExtForeignToplevelImageCaptureSourceManagerV1: ()
        ] => $crate::protocols::image_copy_capture::ImageCopyCaptureState);

        // ext_output_image_capture_source_manager_v1

        smithay::reexports::wayland_server::delegate_global_dispatch!($(@< $( $lt $( : $clt $(+ $dlt )* )? ),+ >)? $ty: [
            smithay::reexports::wayland_protocols::ext::image_capture_source::v1::server::ext_output_image_capture_source_manager_v1::ExtOutputImageCaptureSourceManagerV1: $crate::protocols::image_copy_capture::ImageCaptureSourceGlobalData
        ] => $crate::protocols::image_copy_capture::ImageCopyCaptureState);

        smithay::reexports::wayland_server::delegate_dispatch!($(@< $( $lt $( : $clt $(+ $dlt )* )? ),+ >)? $ty: [
            smithay::reexports::wayland_protocols::ext::image_capture_source::v1::server::ext_output_image_capture_source_manager_v1::ExtOutputImageCaptureSourceManagerV1: ()
        ] => $crate::protocols::image_copy_capture::ImageCopyCaptureState);

        // ext_image_copy_capture_manager_v1

        smithay::reexports::wayland_server::delegate_global_dispatch!($(@< $( $lt $( : $clt $(+ $dlt )* )? ),+ >)? $ty: [
            smithay::reexports::wayland_protocols::ext::image_copy_capture::v1::server::ext_image_copy_capture_manager_v1::ExtImageCopyCaptureManagerV1: ()
        ] => $crate::protocols::image_copy_capture::ImageCopyCaptureState);

        smithay::reexports::wayland_server::delegate_dispatch!($(@< $( $lt $( : $clt $(+ $dlt )* )? ),+ >)? $ty: [
            smithay::reexports::wayland_protocols::ext::image_copy_capture::v1::server::ext_image_copy_capture_manager_v1::ExtImageCopyCaptureManagerV1: ()
        ] => $crate::protocols::image_copy_capture::ImageCopyCaptureState);

        // ext_image_copy_capture_session_v1

        smithay::reexports::wayland_server::delegate_dispatch!($(@< $( $lt $( : $clt $(+ $dlt )* )? ),+ >)? $ty: [
            smithay::reexports::wayland_protocols::ext::image_copy_capture::v1::server::ext_image_copy_capture_session_v1::ExtImageCopyCaptureSessionV1: $crate::protocols::image_copy_capture::ImageCopyCaptureSessionData
        ] => $crate::protocols::image_copy_capture::ImageCopyCaptureState);

        // ext_image_copy_capture_frame_v1

        smithay::reexports::wayland_server::delegate_dispatch!($(@< $( $lt $( : $clt $(+ $dlt )* )? ),+ >)? $ty: [
            smithay::reexports::wayland_protocols::ext::image_copy_capture::v1::server::ext_image_copy_capture_frame_v1::ExtImageCopyCaptureFrameV1: $crate::protocols::image_copy_capture::ImageCopyCaptureFrameData
        ] => $crate::protocols::image_copy_capture::ImageCopyCaptureState);

        // ext_image_copy_capture_cursor_session_v1

        smithay::reexports::wayland_server::delegate_dispatch!($(@< $( $lt $( : $clt $(+ $dlt )* )? ),+ >)? $ty: [
            smithay::reexports::wayland_protocols::ext::image_copy_capture::v1::server::ext_image_copy_capture_cursor_session_v1::ExtImageCopyCaptureCursorSessionV1: $crate::protocols::image_copy_capture::ImageCopyCaptureCursorSessionData
        ] => $crate::protocols::image_copy_capture::ImageCopyCaptureState);
    };
}
