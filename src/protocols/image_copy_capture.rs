use std::sync::atomic::AtomicBool;

use smithay::{backend::{allocator::{Fourcc, Modifier}, drm::DrmNode}, output::{Output, WeakOutput}, reexports::{wayland_protocols::ext::{image_capture_source::v1::server::{ext_foreign_toplevel_image_capture_source_manager_v1::{self, ExtForeignToplevelImageCaptureSourceManagerV1}, ext_image_capture_source_v1::{self, ExtImageCaptureSourceV1}, ext_output_image_capture_source_manager_v1::{self, ExtOutputImageCaptureSourceManagerV1}}, image_copy_capture::v1::server::{ext_image_copy_capture_cursor_session_v1::{self, ExtImageCopyCaptureCursorSessionV1}, ext_image_copy_capture_frame_v1::{self, ExtImageCopyCaptureFrameV1, FailureReason}, ext_image_copy_capture_manager_v1::{self, ExtImageCopyCaptureManagerV1}, ext_image_copy_capture_session_v1::{self, ExtImageCopyCaptureSessionV1}}}, wayland_server::{protocol::wl_shm, Client, DataInit, Dispatch, DisplayHandle, GlobalDispatch, New, Resource}}, utils::{Buffer, Size}};

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

    fn capture_source(&mut self, source: &ImageCaptureSource) -> Option<BufferConstraints>;
    fn capture_cursor_source(&mut self, source: &ImageCaptureSource) -> Option<BufferConstraints>;

    fn new_session(&mut self, session: Session);
    fn new_cursor_session(&mut self, session: CursorSession);

    fn frame(&mut self, session: Session, frame: Frame);
    fn cursor_frame(&mut self, session: CursorSession, frame: Frame);

    fn frame_aborted(&mut self, frame: Frame);

    fn session_destroyed(&mut self, session: Session) {
        let _ = session;
    }
    fn cursor_session_destroyed(&mut self, session: CursorSession) {
        let _ = session;
    }
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
    sessions: Vec<Session>,
    cursor_sessions: Vec<CursorSession>,
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

        // display.create_global::<D, ExtImageCopyCaptureManagerV1, _>(
        //     IMAGE_COPY_CAPTURE_VERSION,
        //     (),
        // );

        Self {
            sessions: Vec::new(),
            cursor_sessions: Vec::new(),
        }
    }
}

#[derive(Debug, Clone)]
pub struct BufferConstraints {
    pub size: Size<i32, Buffer>,
    pub shm: Vec<wl_shm::Format>,
    pub dma: Option<DmabufConstraints>,
}

#[derive(Debug, Clone)]
pub struct DmabufConstraints {
    pub node: DrmNode,
    pub formats: Vec<(Fourcc, Vec<Modifier>)>,
}

pub struct Session {
    obj: ExtImageCopyCaptureSessionV1,
    source: ImageCaptureSource,

    stopped: bool,
    draw_cursors: bool,

    constraints: Option<BufferConstraints>,

    frame: Option<Frame>,
}

pub struct CursorSession {
    obj: ExtImageCopyCaptureCursorSessionV1,
    capture_session: Option<ExtImageCopyCaptureSessionV1>,
    source: ImageCaptureSource,

    stopped: bool,

    constraints: Option<BufferConstraints>,

    frame: Option<Frame>,
}

pub struct Frame {
    obj: ExtImageCopyCaptureFrameV1,
}

impl From<ExtImageCopyCaptureFrameV1> for Frame {
    fn from(obj: ExtImageCopyCaptureFrameV1) -> Self {
        Self { obj }
    }
}

impl Session {
    pub fn stop(mut self) {
        if !self.obj.is_alive() || self.stopped {
            return;
        }

        if let Some(frame) = self.frame.take() {
            frame.fail(FailureReason::Stopped);
        }

        self.obj.stopped();
    }
}

impl CursorSession {
    pub fn stop(mut self) {
        if !self.obj.is_alive() || self.stopped {
            return;
        }

        if let Some(frame) = self.frame.take() {
            frame.fail(FailureReason::Stopped);
        }

        if let Some(capture_session) = self.capture_session.as_ref() {
            capture_session.stopped();
        }
    }
}

impl Frame {
    pub fn fail(self, reason: FailureReason) {
        if !self.obj.is_alive() {
            return;
        }

        self.obj.failed(reason);
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
    D: Dispatch<ExtForeignToplevelImageCaptureSourceManagerV1, ()>,
{
    fn bind(
        _state: &mut D,
        _handle: &DisplayHandle,
        _client: &Client,
        resource: New<ExtForeignToplevelImageCaptureSourceManagerV1>,
        _global_data: &ImageCaptureSourceGlobalData,
        data_init: &mut DataInit<'_, D>,
    ) {
        data_init.init(resource, ());
    }

    fn can_view(client: Client, global_data: &ImageCaptureSourceGlobalData) -> bool {
        (global_data.filter)(&client)
    }
}

impl<D> Dispatch<ExtForeignToplevelImageCaptureSourceManagerV1, (), D> for ImageCopyCaptureState
where
    D: ForeignToplevelHandler,
    D: Dispatch<ExtImageCaptureSourceV1, ImageCaptureSource>,
{
    fn request(
        state: &mut D,
        _client: &Client,
        _resource: &ExtForeignToplevelImageCaptureSourceManagerV1,
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
            ext_foreign_toplevel_image_capture_source_manager_v1::Request::Destroy => {}
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
    D: Dispatch<ExtOutputImageCaptureSourceManagerV1, ()>,
{
    fn bind(
        _state: &mut D,
        _handle: &DisplayHandle,
        _client: &Client,
        resource: New<ExtOutputImageCaptureSourceManagerV1>,
        _global_data: &ImageCaptureSourceGlobalData,
        data_init: &mut DataInit<'_, D>,
    ) {
        data_init.init(resource, ());
    }

    fn can_view(client: Client, global_data: &ImageCaptureSourceGlobalData) -> bool {
        (global_data.filter)(&client)
    }
}

impl<D> Dispatch<ExtOutputImageCaptureSourceManagerV1, (), D> for ImageCopyCaptureState
where
    D: Dispatch<ExtImageCaptureSourceV1, ImageCaptureSource>,
{
    fn request(
        _state: &mut D,
        _client: &Client,
        _resource: &ExtOutputImageCaptureSourceManagerV1,
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
            ext_output_image_capture_source_manager_v1::Request::Destroy => {}
            _ => unreachable!(),
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
    D: Dispatch<ExtImageCopyCaptureManagerV1, ()>,
{
    fn bind(
        _state: &mut D,
        _handle: &DisplayHandle,
        _client: &Client,
        resource: New<ExtImageCopyCaptureManagerV1>,
        _global_data: &(),
        data_init: &mut DataInit<'_, D>,
    ) {
        data_init.init(resource, ());
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
                if let Some(source) = source.data::<ImageCaptureSource>() {
                    if let Some(constraints) = state.capture_source(source) {}
                }
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

pub struct ImageCopyCaptureSessionData {
    stopped: AtomicBool,
}

impl<D> Dispatch<ExtImageCopyCaptureSessionV1, ImageCopyCaptureSessionData, D>
    for ImageCopyCaptureState
where
    D: ImageCopyCaptureHandler,
    D: Dispatch<ExtImageCopyCaptureFrameV1, ImageCopyCaptureInertFrame>,
    D: Dispatch<ExtImageCopyCaptureFrameV1, ImageCopyCaptureNormalFrame>,
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
                if let Some(session) = state
                    .image_capture_state()
                    .sessions
                    .iter_mut()
                    .find(|session| &session.obj == resource)
                {
                    if session.frame.is_some() {
                        data_init.init(frame, ImageCopyCaptureInertFrame);
                        resource.post_error(
                            ext_image_copy_capture_session_v1::Error::DuplicateFrame,
                            "create_frame sent before destroying the previous frame",
                        );
                        return;
                    }

                    let frame = data_init.init(frame, ImageCopyCaptureNormalFrame);

                    session.frame = Some(Frame::from(frame));
                } else {
                    let frame = data_init.init(frame, ImageCopyCaptureInertFrame);
                    frame.failed(FailureReason::Stopped);
                }
            }
            ext_image_copy_capture_session_v1::Request::Destroy => {}
            _ => unreachable!(),
        }
    }

    fn destroyed(
        state: &mut D,
        _client: wayland_backend::server::ClientId,
        resource: &ExtImageCopyCaptureSessionV1,
        _data: &ImageCopyCaptureSessionData,
    ) {
        if let Some(pos) = state
            .image_capture_state()
            .sessions
            .iter()
            .position(|session| &session.obj == resource)
        {
            let session = state.image_capture_state().sessions.remove(pos);
            state.session_destroyed(session);
        }
    }
}

pub struct ImageCopyCaptureCursorSessionData;

impl<D> Dispatch<ExtImageCopyCaptureSessionV1, ImageCopyCaptureCursorSessionData, D>
    for ImageCopyCaptureState
where
    D: ImageCopyCaptureHandler,
{
    fn request(
        state: &mut D,
        client: &Client,
        resource: &ExtImageCopyCaptureSessionV1,
        request: <ExtImageCopyCaptureSessionV1 as Resource>::Request,
        _: &ImageCopyCaptureCursorSessionData,
        dhandle: &DisplayHandle,
        data_init: &mut DataInit<'_, D>,
    ) {
        match request {
            ext_image_copy_capture_session_v1::Request::CreateFrame { frame } => {
                if let Some(cursor_session) = state
                    .image_capture_state()
                    .cursor_sessions
                    .iter_mut()
                    .find(|session| session.capture_session.as_ref() == Some(resource))
                {
                }
            }
            ext_image_copy_capture_session_v1::Request::Destroy => {
                if let Some(cursor_session) = state
                    .image_capture_state()
                    .cursor_sessions
                    .iter_mut()
                    .find(|session| session.capture_session.as_ref() == Some(resource))
                {
                    cursor_session.capture_session.take();
                }
            }
            _ => unreachable!(),
        }
    }
}

impl<D> Dispatch<ExtImageCopyCaptureCursorSessionV1, ImageCopyCaptureCursorSessionData, D>
    for ImageCopyCaptureState
where
    D: ImageCopyCaptureHandler,
    D: Dispatch<ExtImageCopyCaptureSessionV1, ImageCopyCaptureCursorSessionData>,
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
        let Some(cursor_session) = state
            .image_capture_state()
            .cursor_sessions
            .iter_mut()
            .find(|session| &session.obj == resource)
        else {
            return;
        };

        match request {
            ext_image_copy_capture_cursor_session_v1::Request::Destroy => {}
            ext_image_copy_capture_cursor_session_v1::Request::GetCaptureSession { session } => {
                if cursor_session.capture_session.is_none() {
                    cursor_session.capture_session =
                        Some(data_init.init(session, ImageCopyCaptureCursorSessionData));
                }
            }
            _ => unreachable!(),
        }
    }

    fn destroyed(
        state: &mut D,
        _client: wayland_backend::server::ClientId,
        resource: &ExtImageCopyCaptureCursorSessionV1,
        _data: &ImageCopyCaptureCursorSessionData,
    ) {
        if let Some(idx) = state
            .image_capture_state()
            .cursor_sessions
            .iter()
            .position(|session| &session.obj == resource)
        {
            let session = state.image_capture_state().cursor_sessions.remove(idx);
            state.cursor_session_destroyed(session);
        }
    }
}

pub struct ImageCopyCaptureInertFrame;
pub struct ImageCopyCaptureNormalFrame;
pub struct ImageCopyCaptureCursorFrame;

impl<D> Dispatch<ExtImageCopyCaptureFrameV1, ImageCopyCaptureInertFrame, D>
    for ImageCopyCaptureState
{
    fn request(
        state: &mut D,
        client: &Client,
        resource: &ExtImageCopyCaptureFrameV1,
        request: <ExtImageCopyCaptureFrameV1 as smithay::reexports::wayland_server::Resource>::Request,
        data: &ImageCopyCaptureInertFrame,
        dhandle: &DisplayHandle,
        data_init: &mut DataInit<'_, D>,
    ) {
        match request {
            ext_image_copy_capture_frame_v1::Request::Destroy => {}
            ext_image_copy_capture_frame_v1::Request::AttachBuffer { buffer } => {
                trace!("Client attached a buffer to an inert frame. Ignoring.")
            }
            ext_image_copy_capture_frame_v1::Request::DamageBuffer {
                x,
                y,
                width,
                height,
            } => {
                trace!("Client requested damage to an inert frame. Ignoring.")
            }
            ext_image_copy_capture_frame_v1::Request::Capture => {
                trace!("Client requested to capture an inert frame. Ignoring.")
            }
            _ => unreachable!(),
        }
    }
}

impl<D> Dispatch<ExtImageCopyCaptureFrameV1, ImageCopyCaptureNormalFrame, D>
    for ImageCopyCaptureState
{
    fn request(
        state: &mut D,
        client: &Client,
        resource: &ExtImageCopyCaptureFrameV1,
        request: <ExtImageCopyCaptureFrameV1 as smithay::reexports::wayland_server::Resource>::Request,
        data: &ImageCopyCaptureNormalFrame,
        dhandle: &DisplayHandle,
        data_init: &mut DataInit<'_, D>,
    ) {
        match request {
            ext_image_copy_capture_frame_v1::Request::Destroy => todo!(),
            ext_image_copy_capture_frame_v1::Request::AttachBuffer { buffer } => todo!(),
            ext_image_copy_capture_frame_v1::Request::DamageBuffer {
                x,
                y,
                width,
                height,
            } => todo!(),
            ext_image_copy_capture_frame_v1::Request::Capture => todo!(),
            _ => unreachable!(),
        }
    }
}

impl<D> Dispatch<ExtImageCopyCaptureFrameV1, ImageCopyCaptureCursorFrame, D>
    for ImageCopyCaptureState
{
    fn request(
        state: &mut D,
        client: &Client,
        resource: &ExtImageCopyCaptureFrameV1,
        request: <ExtImageCopyCaptureFrameV1 as smithay::reexports::wayland_server::Resource>::Request,
        data: &ImageCopyCaptureCursorFrame,
        dhandle: &DisplayHandle,
        data_init: &mut DataInit<'_, D>,
    ) {
        match request {
            ext_image_copy_capture_frame_v1::Request::Destroy => {}
            ext_image_copy_capture_frame_v1::Request::AttachBuffer { buffer } => {
                trace!("Client attached a buffer to a cursor frame. Ignoring.")
            }
            ext_image_copy_capture_frame_v1::Request::DamageBuffer {
                x,
                y,
                width,
                height,
            } => {
                trace!("Client requested damage to a cursor frame. Ignoring.")
            }
            ext_image_copy_capture_frame_v1::Request::Capture => {
                trace!("Client requested to capture a cursor frame. Ignoring.")
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

        smithay::reexports::wayland_server::delegate_dispatch!($(@< $( $lt $( : $clt $(+ $dlt )* )? ),+ >)? $ty: [
            smithay::reexports::wayland_protocols::ext::image_copy_capture::v1::server::ext_image_copy_capture_session_v1::ExtImageCopyCaptureSessionV1: $crate::protocols::image_copy_capture::ImageCopyCaptureCursorSessionData
        ] => $crate::protocols::image_copy_capture::ImageCopyCaptureState);

        // ext_image_copy_capture_frame_v1

        smithay::reexports::wayland_server::delegate_dispatch!($(@< $( $lt $( : $clt $(+ $dlt )* )? ),+ >)? $ty: [
            smithay::reexports::wayland_protocols::ext::image_copy_capture::v1::server::ext_image_copy_capture_frame_v1::ExtImageCopyCaptureFrameV1: $crate::protocols::image_copy_capture::ImageCopyCaptureInertFrame
        ] => $crate::protocols::image_copy_capture::ImageCopyCaptureState);

        smithay::reexports::wayland_server::delegate_dispatch!($(@< $( $lt $( : $clt $(+ $dlt )* )? ),+ >)? $ty: [
            smithay::reexports::wayland_protocols::ext::image_copy_capture::v1::server::ext_image_copy_capture_frame_v1::ExtImageCopyCaptureFrameV1: $crate::protocols::image_copy_capture::ImageCopyCaptureNormalFrame
        ] => $crate::protocols::image_copy_capture::ImageCopyCaptureState);

        smithay::reexports::wayland_server::delegate_dispatch!($(@< $( $lt $( : $clt $(+ $dlt )* )? ),+ >)? $ty: [
            smithay::reexports::wayland_protocols::ext::image_copy_capture::v1::server::ext_image_copy_capture_frame_v1::ExtImageCopyCaptureFrameV1: $crate::protocols::image_copy_capture::ImageCopyCaptureCursorFrame
        ] => $crate::protocols::image_copy_capture::ImageCopyCaptureState);

        // ext_image_copy_capture_cursor_session_v1

        smithay::reexports::wayland_server::delegate_dispatch!($(@< $( $lt $( : $clt $(+ $dlt )* )? ),+ >)? $ty: [
            smithay::reexports::wayland_protocols::ext::image_copy_capture::v1::server::ext_image_copy_capture_cursor_session_v1::ExtImageCopyCaptureCursorSessionV1: $crate::protocols::image_copy_capture::ImageCopyCaptureCursorSessionData
        ] => $crate::protocols::image_copy_capture::ImageCopyCaptureState);
    };
}
