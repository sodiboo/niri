use std::{sync::atomic::AtomicBool, time::Duration};

use futures_util::io::Cursor;
use smithay::{backend::{allocator::{Buffer as _, Fourcc, Modifier}, drm::DrmNode, renderer::{buffer_type, damage::OutputDamageTracker, BufferType}}, output::{Output, WeakOutput}, reexports::{wayland_protocols::ext::{image_capture_source::v1::server::{ext_foreign_toplevel_image_capture_source_manager_v1::{self, ExtForeignToplevelImageCaptureSourceManagerV1}, ext_image_capture_source_v1::{self, ExtImageCaptureSourceV1}, ext_output_image_capture_source_manager_v1::{self, ExtOutputImageCaptureSourceManagerV1}}, image_copy_capture::v1::server::{ext_image_copy_capture_cursor_session_v1::{self, ExtImageCopyCaptureCursorSessionV1}, ext_image_copy_capture_frame_v1::{self, ExtImageCopyCaptureFrameV1, FailureReason}, ext_image_copy_capture_manager_v1::{self, ExtImageCopyCaptureManagerV1}, ext_image_copy_capture_session_v1::{self, ExtImageCopyCaptureSessionV1}}}, wayland_server::{protocol::{wl_buffer::WlBuffer, wl_pointer::WlPointer, wl_shm}, Client, DataInit, Dispatch, DisplayHandle, GlobalDispatch, New, Resource}}, utils::{Buffer, Rectangle, Size, Transform}, wayland::{dmabuf::get_dmabuf, shm::with_buffer_contents}};

use crate::window::mapped::MappedId;

use super::foreign_toplevel::ForeignToplevelHandler;

const FOREIGN_TOPLEVEL_SOURCE_VERSION: u32 = 1;
const OUTPUT_SOURCE_VERSION: u32 = 1;
const IMAGE_COPY_CAPTURE_VERSION: u32 = 1;

#[derive(Debug, Clone)]
pub enum ImageCaptureSource {
    Invalid,
    Toplevel(MappedId),
    Output(WeakOutput),
}

pub trait ImageCopyCaptureHandler {
    fn image_capture_state(&mut self) -> &mut ImageCopyCaptureState;

    fn capture_source(&mut self, source: &ImageCaptureSource) -> Option<BufferConstraints>;
    fn capture_cursor_source(&mut self, source: &ImageCaptureSource) -> Option<BufferConstraints>;

    fn new_session(&mut self, session: ExtImageCopyCaptureSessionV1);
    fn new_cursor_session(&mut self, session: ExtImageCopyCaptureCursorSessionV1);

    fn frame_requested(
        &mut self,
        session: ExtImageCopyCaptureSessionV1,
        frame: ExtImageCopyCaptureFrameV1,
    );
    fn cursor_frame_requested(
        &mut self,
        session: ExtImageCopyCaptureCursorSessionV1,
        frame: ExtImageCopyCaptureFrameV1,
    );

    fn frame_aborted(&mut self, frame: ExtImageCopyCaptureFrameV1);
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

        display.create_global::<D, ExtImageCopyCaptureManagerV1, _>(IMAGE_COPY_CAPTURE_VERSION, ());

        Self {
            sessions: Vec::new(),
            cursor_sessions: Vec::new(),
        }
    }

    pub fn get_session_mut(
        &mut self,
        resource: &ExtImageCopyCaptureSessionV1,
    ) -> Option<&mut Session> {
        self.sessions
            .iter_mut()
            .find(|session| &session.resource == resource)
    }

    pub fn get_cursor_session_mut(
        &mut self,
        resource: &ExtImageCopyCaptureCursorSessionV1,
    ) -> Option<&mut CursorSession> {
        self.cursor_sessions
            .iter_mut()
            .find(|session| &session.resource == resource)
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
    resource: ExtImageCopyCaptureSessionV1,
    source: ImageCaptureSource,

    stopped: bool,
    draw_cursors: bool,

    pub damage_tracker: Option<OutputDamageTracker>,

    constraints: BufferConstraints,

    frame: Option<Frame>,
}

pub struct CursorSession {
    resource: ExtImageCopyCaptureCursorSessionV1,
    source: ImageCaptureSource,
    pointer: WlPointer,

    capture_session: Option<ExtImageCopyCaptureSessionV1>,

    stopped: bool,

    constraints: BufferConstraints,

    frame: Option<Frame>,
}

pub struct Frame {
    resource: ExtImageCopyCaptureFrameV1,

    capture_requested: bool,
    result: Option<FrameResult>,

    buffer: Option<WlBuffer>,
    damage: Vec<Rectangle<i32, Buffer>>,
}

#[derive(Debug, Clone, Copy)]
enum FrameResult {
    Sucess,
    Failure(FailureReason),
}

impl From<ExtImageCopyCaptureFrameV1> for Frame {
    fn from(obj: ExtImageCopyCaptureFrameV1) -> Self {
        Self {
            resource: obj,
            capture_requested: false,
            result: None,
            buffer: None,
            damage: Vec::new(),
        }
    }
}

impl BufferConstraints {
    fn send_to(&self, session: &ExtImageCopyCaptureSessionV1) {
        session.buffer_size(self.size.w as u32, self.size.h as u32);

        for &fmt in &self.shm {
            session.shm_format(fmt);
        }

        if let Some(dma) = self.dma.as_ref() {
            session.dmabuf_device(dma.node.dev_id().to_ne_bytes().to_vec());

            for &(fmt, ref modifiers) in &dma.formats {
                let modifiers: Vec<u8> = modifiers
                    .iter()
                    .cloned()
                    .map(u64::from)
                    .flat_map(u64::to_ne_bytes)
                    .collect();
                session.dmabuf_format(fmt as u32, modifiers);
            }
        }

        session.done();
    }
}

impl Session {
    fn ended(&self) -> bool {
        !self.resource.is_alive() || self.stopped
    }

    pub fn source(&self) -> ImageCaptureSource {
        self.source.clone()
    }

    pub fn with_frame<T>(&mut self, f: impl FnOnce(&mut Frame) -> T) -> Option<T> {
        self.frame.as_mut().map(f)
    }

    pub fn advertise_constraints(&self) {
        if self.ended() {
            return;
        }

        self.constraints.send_to(&self.resource);
    }

    pub fn update_constraints(&mut self, constraints: BufferConstraints) {
        self.constraints = constraints;
        self.advertise_constraints();
    }

    pub fn stop(&mut self) {
        if self.stopped {
            error!("Capture session stopped twice.");
        }
        if let Some(mut frame) = self.frame.take() {
            frame.fail(FailureReason::Stopped);
        }

        if self.resource.is_alive() && !self.stopped {
            self.resource.stopped();
        }
        self.stopped = true;
    }
}

impl CursorSession {
    fn ended(&self) -> bool {
        !self.resource.is_alive() || self.stopped
    }

    pub fn advertise_constraints(&self) {
        if self.ended() {
            return;
        }

        if let Some(capture_session) = self.capture_session.as_ref() {
            self.constraints.send_to(capture_session);
        }
    }

    pub fn update_constraints(&mut self, constraints: BufferConstraints) {
        self.constraints = constraints;
        self.advertise_constraints();
    }

    fn init_capture_session(&mut self, capture_session: ExtImageCopyCaptureSessionV1) {
        if self.capture_session.is_some() {
            error!("Cursor session already has a capture session.");
            return;
        }

        self.capture_session = Some(capture_session.clone());

        self.advertise_constraints();

        if self.stopped {
            capture_session.stopped();
        }
    }

    pub fn stop(&mut self) {
        if self.stopped {
            error!("Cursor session stopped twice.");
        }

        if let Some(mut frame) = self.frame.take() {
            frame.fail(FailureReason::Stopped);
        }

        if let Some(capture_session) = self.capture_session.as_ref() {
            if capture_session.is_alive() {
                capture_session.stopped();
            }
        }

        self.stopped = true;
    }
}

impl Frame {
    pub fn should_render(&self) -> bool {
        self.result.is_none() && self.capture_requested
    }

    fn capture(&mut self, constraints: &BufferConstraints) {
        if self.capture_requested {
            self.resource.post_error(
                ext_image_copy_capture_frame_v1::Error::AlreadyCaptured,
                "Frame was captured previously",
            );
        }
        self.capture_requested = true;

        let Some(buffer) = self.buffer.as_ref() else {
            self.resource.post_error(
                ext_image_copy_capture_frame_v1::Error::NoBuffer,
                "Attempting to capture frame without a buffer",
            );
            return;
        };

        if let Some(result) = self.result {
            match result {
                FrameResult::Sucess => {
                    error!("Frame was successful before a capture? This is unreachable.");
                    self.resource.failed(FailureReason::Unknown);
                }
                FrameResult::Failure(reason) => {
                    self.resource.failed(reason);
                }
            }
            return;
        }

        match buffer_type(buffer) {
            Some(BufferType::Dma) => {
                let Some(dma_constraints) = constraints.dma.as_ref() else {
                    debug!("dma buffer not specified for image-copy-capture");
                    self.fail(FailureReason::BufferConstraints);
                    return;
                };

                let dmabuf = match get_dmabuf(buffer) {
                    Ok(buf) => buf,
                    Err(err) => {
                        debug!(?err, "Error accessing dma buffer for image-copy-capture");
                        self.fail(FailureReason::Unknown);
                        return;
                    }
                };

                let buffer_size = dmabuf.size();
                if buffer_size.w < constraints.size.w || buffer_size.h < constraints.size.h {
                    debug!(?buffer_size, ?constraints.size, "buffer too small for image-copy-capture");
                    self.fail(FailureReason::BufferConstraints);
                    return;
                }

                let format = dmabuf.format();
                if dma_constraints
                    .formats
                    .iter()
                    .find(|(fourcc, _)| *fourcc == format.code)
                    .filter(|(_, modifiers)| modifiers.contains(&format.modifier))
                    .is_none()
                {
                    debug!(
                        ?format,
                        ?dma_constraints,
                        "unsupported buffer format for image-copy-capture"
                    );
                    self.fail(FailureReason::BufferConstraints);
                    return;
                }
            }
            Some(BufferType::Shm) => {
                let buffer_data = match with_buffer_contents(buffer, |_, _, data| data) {
                    Ok(data) => data,
                    Err(err) => {
                        debug!(?err, "Error accessing shm buffer for image-copy-capture");
                        self.fail(FailureReason::Unknown);
                        return;
                    }
                };

                if buffer_data.width < constraints.size.w || buffer_data.height < constraints.size.h
                {
                    debug!(?buffer_data, ?constraints.size, "buffer too small for image-copy-capture");
                    self.fail(FailureReason::BufferConstraints);
                    return;
                }

                if !constraints.shm.contains(&buffer_data.format) {
                    debug!(?buffer_data.format, ?constraints.shm, "unsupported buffer format for image-copy-capture");
                    self.fail(FailureReason::BufferConstraints);
                    return;
                }
            }
            x => {
                debug!(?x, "Attempt to capture with unsupported buffer type");
                self.fail(FailureReason::BufferConstraints);
                return;
            }
        }
    }

    pub fn success(
        &mut self,
        transform: Transform,
        damage: impl Into<Option<Vec<Rectangle<i32, Buffer>>>>,
        presented: impl Into<Duration>,
    ) {
        if self.result.is_some() {
            error!("Frame was processed already.");
            return;
        }

        if !self.capture_requested {
            error!("Frame was not requested to be captured.");
            self.result = Some(FrameResult::Failure(FailureReason::Unknown));
            return;
        }

        if !self.resource.is_alive() {
            return;
        }

        self.resource.transform(transform.into());

        for damage in damage.into().into_iter().flatten() {
            self.resource
                .damage(damage.loc.x, damage.loc.y, damage.size.w, damage.size.h);
        }

        let time = presented.into();

        let tv_sec_hi = (time.as_secs() >> 32) as u32;
        let tv_sec_lo = time.as_secs() as u32;
        let tv_nsec = time.subsec_nanos();

        self.resource
            .presentation_time(tv_sec_hi, tv_sec_lo, tv_nsec);

        self.resource.ready();

        self.result = Some(FrameResult::Sucess);
    }

    pub fn fail(&mut self, reason: FailureReason) {
        if !self.resource.is_alive() {
            return;
        }

        if self.result.is_some() {
            error!("Frame was processed already.");
            return;
        }

        self.result = Some(FrameResult::Failure(reason));

        if self.capture_requested {
            self.resource.failed(reason);
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
    D: Dispatch<ExtImageCopyCaptureSessionV1, ImageCopyCaptureSessionData>,
    D: Dispatch<ExtImageCopyCaptureCursorSessionV1, ImageCopyCaptureCursorSessionData>,
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
                let Some(options) =
                    ext_image_copy_capture_manager_v1::Options::from_bits(options.into())
                else {
                    resource.post_error(
                        ext_image_copy_capture_manager_v1::Error::InvalidOption,
                        "invalid options",
                    );
                    return;
                };

                let draw_cursors =
                    options.contains(ext_image_copy_capture_manager_v1::Options::PaintCursors);

                if let Some(source) = source.data::<ImageCaptureSource>() {
                    if let Some(constraints) = state.capture_source(source) {
                        let resource = data_init.init(session, ImageCopyCaptureSessionData);
                        let session = Session {
                            resource: resource.clone(),
                            source: source.clone(),
                            stopped: false,
                            draw_cursors,
                            damage_tracker: None,
                            constraints,
                            frame: None,
                        };
                        session.advertise_constraints();

                        state.image_capture_state().sessions.push(session);

                        state.new_session(resource);
                        return;
                    }
                }

                let mut session = Session {
                    resource: data_init.init(session, ImageCopyCaptureSessionData),
                    source: ImageCaptureSource::Invalid,
                    stopped: false,
                    draw_cursors,
                    damage_tracker: None,
                    constraints: BufferConstraints {
                        size: (0, 0).into(),
                        shm: Vec::new(),
                        dma: None,
                    },
                    frame: None,
                };
                session.advertise_constraints();
                session.stop();
            }
            ext_image_copy_capture_manager_v1::Request::CreatePointerCursorSession {
                session,
                source,
                pointer,
            } => {
                if let Some(source) = source.data::<ImageCaptureSource>() {
                    if let Some(constraints) = state.capture_cursor_source(source) {
                        let resource = data_init.init(session, ImageCopyCaptureCursorSessionData);
                        let cursor_session = CursorSession {
                            resource: resource.clone(),
                            source: source.clone(),
                            pointer,
                            capture_session: None,
                            stopped: false,
                            constraints,
                            frame: None,
                        };
                        cursor_session.advertise_constraints();

                        state
                            .image_capture_state()
                            .cursor_sessions
                            .push(cursor_session);
                        state.new_cursor_session(resource);
                        return;
                    }
                }

                let mut cursor_session = CursorSession {
                    resource: data_init.init(session, ImageCopyCaptureCursorSessionData),
                    source: ImageCaptureSource::Invalid,
                    pointer,
                    capture_session: None,
                    stopped: false,
                    constraints: BufferConstraints {
                        size: (0, 0).into(),
                        shm: Vec::new(),
                        dma: None,
                    },
                    frame: None,
                };
                cursor_session.advertise_constraints();

                cursor_session.stop();
            }
            ext_image_copy_capture_manager_v1::Request::Destroy => {}
            _ => unreachable!(),
        }
    }
}

pub struct ImageCopyCaptureSessionData;

impl<D> Dispatch<ExtImageCopyCaptureSessionV1, ImageCopyCaptureSessionData, D>
    for ImageCopyCaptureState
where
    D: ImageCopyCaptureHandler,
    D: Dispatch<ExtImageCopyCaptureFrameV1, ImageCopyCaptureNormalFrame>,
    D: Dispatch<ExtImageCopyCaptureFrameV1, ImageCopyCaptureInertFrame>,
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
                    .find(|session| &session.resource == resource)
                {
                    if session.frame.is_some() {
                        resource.post_error(
                            ext_image_copy_capture_session_v1::Error::DuplicateFrame,
                            "create_frame sent before destroying the previous frame",
                        );
                        return;
                    }

                    let frame = data_init.init(frame, ImageCopyCaptureNormalFrame);

                    session.frame = Some(Frame::from(frame));
                } else {
                    data_init.init(frame, ImageCopyCaptureInertFrame);
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
            .position(|session| &session.resource == resource)
        {
            let mut session = state.image_capture_state().sessions.remove(pos);
            if !session.stopped {
                session.stop();
            }
            state.session_destroyed(session);
        }
    }
}

pub struct ImageCopyCaptureCursorSessionData;

impl<D> Dispatch<ExtImageCopyCaptureSessionV1, ImageCopyCaptureCursorSessionData, D>
    for ImageCopyCaptureState
where
    D: ImageCopyCaptureHandler,
    D: Dispatch<ExtImageCopyCaptureFrameV1, ImageCopyCaptureCursorFrame>,
    D: Dispatch<ExtImageCopyCaptureFrameV1, ImageCopyCaptureInertFrame>,
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
                    if cursor_session.frame.is_some() {
                        resource.post_error(
                            ext_image_copy_capture_session_v1::Error::DuplicateFrame,
                            "create_frame sent before destroying the previous frame",
                        );
                        return;
                    }

                    let frame = data_init.init(frame, ImageCopyCaptureCursorFrame);

                    cursor_session.frame = Some(Frame::from(frame));
                } else {
                    data_init.init(frame, ImageCopyCaptureInertFrame);
                }
            }
            ext_image_copy_capture_session_v1::Request::Destroy => {}
            _ => unreachable!(),
        }
    }

    fn destroyed(
        _state: &mut D,
        _client: wayland_backend::server::ClientId,
        _resource: &ExtImageCopyCaptureSessionV1,
        _data: &ImageCopyCaptureCursorSessionData,
    ) {
        // We don't need to do anything here, it's cleaned up by
        // ExtImageCopyCaptureCursorSessionV1::destroyed
        // and we shouldn't `take()` this session out of the CursorSession,
        // because creating another one ever is a protocol error.
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
        match request {
            ext_image_copy_capture_cursor_session_v1::Request::Destroy => {}
            ext_image_copy_capture_cursor_session_v1::Request::GetCaptureSession { session } => {
                let Some(cursor_session) = state
                    .image_capture_state()
                    .cursor_sessions
                    .iter_mut()
                    .find(|session| &session.resource == resource)
                else {
                    let session = data_init.init(session, ImageCopyCaptureCursorSessionData);

                    let mut session = Session {
                        resource: session,
                        source: ImageCaptureSource::Invalid,
                        stopped: false,
                        draw_cursors: false,
                        damage_tracker: None,
                        constraints: BufferConstraints {
                            size: (0, 0).into(),
                            shm: Vec::new(),
                            dma: None,
                        },
                        frame: None,
                    };
                    session.advertise_constraints();
                    session.stop();
                    return;
                };
                if cursor_session.capture_session.is_some() {
                    resource.post_error(
                        ext_image_copy_capture_cursor_session_v1::Error::DuplicateSession,
                        "get_capture_session called twice for a cursor sessionr",
                    );
                    return;
                }

                cursor_session.init_capture_session(
                    data_init.init(session, ImageCopyCaptureCursorSessionData),
                );
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
            .position(|session| &session.resource == resource)
        {
            let mut session = state.image_capture_state().cursor_sessions.remove(idx);
            if !session.stopped {
                session.stop();
            }
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
        _state: &mut D,
        _client: &Client,
        resource: &ExtImageCopyCaptureFrameV1,
        request: <ExtImageCopyCaptureFrameV1 as smithay::reexports::wayland_server::Resource>::Request,
        _data: &ImageCopyCaptureInertFrame,
        _dhandle: &DisplayHandle,
        _data_init: &mut DataInit<'_, D>,
    ) {
        match request {
            ext_image_copy_capture_frame_v1::Request::Destroy => {}
            ext_image_copy_capture_frame_v1::Request::AttachBuffer { .. } => {}
            ext_image_copy_capture_frame_v1::Request::DamageBuffer { .. } => {}
            ext_image_copy_capture_frame_v1::Request::Capture => {
                resource.failed(FailureReason::Stopped);
            }

            _ => unreachable!(),
        }
    }
}

impl<D> Dispatch<ExtImageCopyCaptureFrameV1, ImageCopyCaptureNormalFrame, D>
    for ImageCopyCaptureState
where
    D: ImageCopyCaptureHandler,
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
        let session = state
            .image_capture_state()
            .sessions
            .iter_mut()
            .find(|session| session.frame.as_ref().map(|frame| &frame.resource) == Some(resource));
        match request {
            ext_image_copy_capture_frame_v1::Request::Destroy => {
                if let Some(session) = session {
                    session.frame = None;
                }
            }
            ext_image_copy_capture_frame_v1::Request::AttachBuffer { buffer } => {
                if let Some(session) = session {
                    let frame = session.frame.as_mut().unwrap();

                    if frame.capture_requested {
                        resource.post_error(
                            ext_image_copy_capture_frame_v1::Error::AlreadyCaptured,
                            "attach_buffer called after capture",
                        );
                        return;
                    }

                    frame.buffer = Some(buffer);
                }
            }
            ext_image_copy_capture_frame_v1::Request::DamageBuffer {
                x,
                y,
                width,
                height,
            } => {
                if x < 0 || y < 0 || width <= 0 || height <= 0 {
                    resource.post_error(
                        ext_image_copy_capture_frame_v1::Error::InvalidBufferDamage,
                        if width == 0 || height == 0 {
                            "damage_buffer with zero area"
                        } else {
                            "damage_buffer with negative dimensions"
                        },
                    );
                    return;
                }

                if let Some(session) = session {
                    let frame: &mut Frame = session.frame.as_mut().unwrap();

                    if frame.capture_requested {
                        resource.post_error(
                            ext_image_copy_capture_frame_v1::Error::AlreadyCaptured,
                            "damage_buffer called after capture",
                        );
                        return;
                    }

                    frame
                        .damage
                        .push(Rectangle::from_loc_and_size((x, y), (width, height)));
                }
            }
            ext_image_copy_capture_frame_v1::Request::Capture => {
                if let Some(session) = session {
                    let frame = session.frame.as_mut().unwrap();

                    if session.stopped {
                        frame.fail(FailureReason::Stopped);
                    } else {
                        frame.capture(&session.constraints);

                        state.frame_requested(session, frame);
                    }
                } else {
                    resource.failed(FailureReason::Stopped);
                }
            }
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
