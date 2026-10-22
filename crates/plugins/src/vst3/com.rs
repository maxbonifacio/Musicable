//! The host's side of VST3: the objects the host makes for plugins, and
//! the ones plugins ask it to make. The ones processing uses are in
//! [`changes`](super::changes).

use std::ffi::{CStr, CString, c_char, c_void};
use std::ptr;
use std::slice;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, Weak};

use vst3::Steinberg::Vst::{
    IAttributeList, IAttributeListTrait, IAudioProcessor, IComponent, IComponentHandler,
    IComponentHandler2, IComponentHandler2Trait, IComponentHandlerTrait, IConnectionPoint,
    IEditController, IHostApplication, IHostApplicationTrait, IMessage, IMessageTrait,
    IPlugInterfaceSupport, IPlugInterfaceSupportTrait, ParamID, ParamValue, String128, TChar,
};
use vst3::Steinberg::{
    FIDString, FUnknown, IBStream, IBStreamTrait, IPlugFrame, IPlugFrameTrait, IPlugView,
    IPlugViewContentScaleSupport, IPlugViewTrait, ISizeableStream, ISizeableStreamTrait, TBool,
    TUID, ViewRect, int32, int64, kInvalidArgument, kNoInterface, kResultFalse, kResultOk,
    kResultTrue, tresult, uint32,
};
use vst3::{Class, ComRef, ComWrapper, Interface};

use super::host::{Edit, Shared};

/// What a plugin knows the host by, which it gets as it initializes.
pub(crate) struct HostApplication;

impl Class for HostApplication {
    type Interfaces = (IHostApplication, IPlugInterfaceSupport);
}

impl IHostApplicationTrait for HostApplication {
    unsafe fn getName(&self, name: *mut String128) -> tresult {
        // SAFETY: the plugin passes somewhere to write, or null.
        let Some(name) = (unsafe { name.as_mut() }) else {
            return kInvalidArgument;
        };
        write_string128(name, "Musicable");
        kResultOk
    }

    unsafe fn createInstance(
        &self,
        cid: *mut TUID,
        iid: *mut TUID,
        object: *mut *mut c_void,
    ) -> tresult {
        if object.is_null() {
            return kInvalidArgument;
        }
        // SAFETY: checked non-null.
        unsafe { object.write(ptr::null_mut()) };
        // SAFETY: the plugin passes ids, or null.
        let (Some(cid), Some(iid)) = (unsafe { cid.as_ref() }, unsafe { iid.as_ref() }) else {
            return kInvalidArgument;
        };
        // Plugins name the class by its interface, as VST3's SDK does.
        let made = if is(cid, &IMessage::IID) {
            ComWrapper::new(Message::default()).to_com_ptr::<FUnknown>()
        } else if is(cid, &IAttributeList::IID) {
            ComWrapper::new(AttributeList::default()).to_com_ptr::<FUnknown>()
        } else {
            None
        };
        let Some(made) = made else {
            return kNoInterface;
        };
        let unknown = made.as_ptr();
        // SAFETY: an object just made, asked for the interface the plugin
        // wants, which then holds a reference of its own.
        unsafe { ((*(*unknown).vtbl).queryInterface)(unknown, iid, object) }
    }
}

impl IPlugInterfaceSupportTrait for HostApplication {
    unsafe fn isPlugInterfaceSupported(&self, iid: *const TUID) -> tresult {
        // SAFETY: the plugin passes an id, or null.
        let Some(iid) = (unsafe { iid.as_ref() }) else {
            return kInvalidArgument;
        };
        let supported = [
            IComponent::IID,
            IAudioProcessor::IID,
            IEditController::IID,
            IConnectionPoint::IID,
            IPlugView::IID,
            IPlugViewContentScaleSupport::IID,
        ];
        if supported.iter().any(|known| is(iid, known)) {
            kResultTrue
        } else {
            kResultFalse
        }
    }
}

/// Whether a plugin's `TUID` is the interface id `iid`.
pub(crate) fn is(id: &TUID, iid: &[u8; 16]) -> bool {
    id.iter().zip(iid).all(|(&a, &b)| a as u8 == b)
}

/// Writes `text` into a VST3 string, cut short if need be.
pub(crate) fn write_string128(target: &mut String128, text: &str) {
    let mut chars = text.encode_utf16();
    let last = target.len() - 1;
    for slot in &mut target[..last] {
        *slot = chars.next().unwrap_or(0);
    }
    target[last] = 0;
}

/// A nul-terminated UTF-16 string a plugin passed.
///
/// # Safety
///
/// `text` must be nul-terminated.
unsafe fn read_tchars(text: *const TChar) -> Vec<TChar> {
    let mut chars = Vec::new();
    for i in 0.. {
        // SAFETY: the caller's promise: it goes on until its nul.
        let c = unsafe { *text.add(i) };
        if c == 0 {
            break;
        }
        chars.push(c);
    }
    chars
}

/// A message between a plugin's component and its controller, which the
/// plugin asks the host for.
pub(crate) struct Message {
    id: Mutex<CString>,
    attributes: ComWrapper<AttributeList>,
}

impl Default for Message {
    fn default() -> Self {
        Self {
            id: Mutex::new(CString::default()),
            attributes: ComWrapper::new(AttributeList::default()),
        }
    }
}

impl Class for Message {
    type Interfaces = (IMessage,);
}

impl IMessageTrait for Message {
    unsafe fn getMessageID(&self) -> FIDString {
        // It stays put until the plugin sets another.
        self.id.lock().unwrap_or_else(|e| e.into_inner()).as_ptr()
    }

    unsafe fn setMessageID(&self, id: FIDString) {
        if id.is_null() {
            return;
        }
        // SAFETY: the plugin passes a nul-terminated id.
        let id = unsafe { CStr::from_ptr(id) }.to_owned();
        *self.id.lock().unwrap_or_else(|e| e.into_inner()) = id;
    }

    unsafe fn getAttributes(&self) -> *mut IAttributeList {
        // The message's own: VST3 hands it out without a reference.
        self.attributes
            .as_com_ref::<IAttributeList>()
            .map_or(ptr::null_mut(), |list| list.as_ptr())
    }
}

/// A value in an [`AttributeList`].
enum Value {
    Int(i64),
    Float(f64),
    /// Without its nul.
    String(Vec<TChar>),
    Binary(Vec<u8>),
}

/// Values by name, which messages carry.
#[derive(Default)]
pub(crate) struct AttributeList {
    values: Mutex<Vec<(CString, Value)>>,
}

impl AttributeList {
    fn set(&self, id: *const c_char, value: Value) -> tresult {
        if id.is_null() {
            return kInvalidArgument;
        }
        // SAFETY: the plugin passes a nul-terminated id.
        let id = unsafe { CStr::from_ptr(id) };
        let mut values = self.values.lock().unwrap_or_else(|e| e.into_inner());
        match values.iter_mut().find(|(name, _)| name.as_c_str() == id) {
            Some((_, old)) => *old = value,
            None => values.push((id.to_owned(), value)),
        }
        kResultOk
    }

    /// Calls `read` with the value named `id`, if there is one.
    fn get<T>(&self, id: *const c_char, read: impl FnOnce(&Value) -> Option<T>) -> Option<T> {
        if id.is_null() {
            return None;
        }
        // SAFETY: the plugin passes a nul-terminated id.
        let id = unsafe { CStr::from_ptr(id) };
        let values = self.values.lock().unwrap_or_else(|e| e.into_inner());
        values
            .iter()
            .find(|(name, _)| name.as_c_str() == id)
            .and_then(|(_, value)| read(value))
    }
}

impl Class for AttributeList {
    type Interfaces = (IAttributeList,);
}

impl IAttributeListTrait for AttributeList {
    unsafe fn setInt(&self, id: *const c_char, value: int64) -> tresult {
        self.set(id, Value::Int(value))
    }

    unsafe fn getInt(&self, id: *const c_char, value: *mut int64) -> tresult {
        let found = self.get(id, |v| match v {
            Value::Int(i) => Some(*i),
            _ => None,
        });
        // SAFETY: the plugin passes somewhere to write, or null.
        match (found, unsafe { value.as_mut() }) {
            (Some(found), Some(value)) => {
                *value = found;
                kResultOk
            }
            _ => kResultFalse,
        }
    }

    unsafe fn setFloat(&self, id: *const c_char, value: f64) -> tresult {
        self.set(id, Value::Float(value))
    }

    unsafe fn getFloat(&self, id: *const c_char, value: *mut f64) -> tresult {
        let found = self.get(id, |v| match v {
            Value::Float(f) => Some(*f),
            _ => None,
        });
        // SAFETY: the plugin passes somewhere to write, or null.
        match (found, unsafe { value.as_mut() }) {
            (Some(found), Some(value)) => {
                *value = found;
                kResultOk
            }
            _ => kResultFalse,
        }
    }

    unsafe fn setString(&self, id: *const c_char, string: *const TChar) -> tresult {
        if string.is_null() {
            return kInvalidArgument;
        }
        // SAFETY: the plugin passes a nul-terminated string.
        self.set(id, Value::String(unsafe { read_tchars(string) }))
    }

    unsafe fn getString(&self, id: *const c_char, string: *mut TChar, size: uint32) -> tresult {
        let capacity = size as usize / size_of::<TChar>();
        if string.is_null() || capacity == 0 {
            return kInvalidArgument;
        }
        let copied = self.get(id, |v| match v {
            Value::String(chars) => {
                let count = chars.len().min(capacity - 1);
                // SAFETY: the plugin passes room for `size` bytes.
                unsafe {
                    ptr::copy_nonoverlapping(chars.as_ptr(), string, count);
                    string.add(count).write(0);
                }
                Some(())
            }
            _ => None,
        });
        if copied.is_some() {
            kResultOk
        } else {
            kResultFalse
        }
    }

    unsafe fn setBinary(&self, id: *const c_char, data: *const c_void, size: uint32) -> tresult {
        let bytes = if size == 0 {
            Vec::new()
        } else if data.is_null() {
            return kInvalidArgument;
        } else {
            // SAFETY: the plugin passes `size` bytes.
            unsafe { slice::from_raw_parts(data.cast::<u8>(), size as usize) }.to_vec()
        };
        self.set(id, Value::Binary(bytes))
    }

    unsafe fn getBinary(
        &self,
        id: *const c_char,
        data: *mut *const c_void,
        size: *mut uint32,
    ) -> tresult {
        if data.is_null() || size.is_null() {
            return kInvalidArgument;
        }
        let found = self.get(id, |v| match v {
            // The bytes stay put for as long as the value does.
            Value::Binary(bytes) => Some((bytes.as_ptr(), bytes.len())),
            _ => None,
        });
        let Some((bytes, len)) = found else {
            return kResultFalse;
        };
        // SAFETY: checked non-null; the plugin passes somewhere to write.
        unsafe {
            data.write(bytes.cast());
            size.write(len.min(u32::MAX as usize) as u32);
        }
        kResultOk
    }
}

/// A plugin's state, as it reads or writes it.
pub(crate) struct Stream {
    /// The bytes, and where the plugin is in them.
    inner: Mutex<(Vec<u8>, usize)>,
}

impl Stream {
    /// A stream to read `bytes` from, or to write to if empty.
    pub(crate) fn new(bytes: &[u8]) -> ComWrapper<Self> {
        ComWrapper::new(Self {
            inner: Mutex::new((bytes.to_vec(), 0)),
        })
    }

    /// Back to the start, to read what was written.
    pub(crate) fn rewind(&self) {
        self.inner.lock().unwrap_or_else(|e| e.into_inner()).1 = 0;
    }

    /// Everything written, taken out.
    pub(crate) fn take(&self) -> Vec<u8> {
        let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        inner.1 = 0;
        std::mem::take(&mut inner.0)
    }

    pub(crate) fn ptr(stream: &ComWrapper<Self>) -> *mut IBStream {
        stream
            .as_com_ref::<IBStream>()
            .map_or(ptr::null_mut(), |s| s.as_ptr())
    }
}

impl Class for Stream {
    type Interfaces = (IBStream, ISizeableStream);
}

impl IBStreamTrait for Stream {
    unsafe fn read(&self, buffer: *mut c_void, count: int32, read: *mut int32) -> tresult {
        if count < 0 || (buffer.is_null() && count > 0) {
            return kInvalidArgument;
        }
        let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        let (bytes, at) = &mut *inner;
        let n = (count as usize).min(bytes.len().saturating_sub(*at));
        if n > 0 {
            // SAFETY: the plugin passes room for `count` bytes.
            unsafe { ptr::copy_nonoverlapping(bytes[*at..].as_ptr(), buffer.cast::<u8>(), n) };
        }
        *at += n;
        if !read.is_null() {
            // SAFETY: checked non-null.
            unsafe { read.write(n as int32) };
        }
        kResultOk
    }

    unsafe fn write(&self, buffer: *mut c_void, count: int32, written: *mut int32) -> tresult {
        if count < 0 || (buffer.is_null() && count > 0) {
            return kInvalidArgument;
        }
        let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        let (bytes, at) = &mut *inner;
        let n = count as usize;
        if n > 0 {
            // SAFETY: the plugin passes `count` bytes.
            let data = unsafe { slice::from_raw_parts(buffer.cast::<u8>(), n) };
            let end = *at + n;
            if bytes.len() < end {
                bytes.resize(end, 0);
            }
            bytes[*at..end].copy_from_slice(data);
            *at = end;
        }
        if !written.is_null() {
            // SAFETY: checked non-null.
            unsafe { written.write(count) };
        }
        kResultOk
    }

    unsafe fn seek(&self, pos: int64, mode: int32, result: *mut int64) -> tresult {
        use vst3::Steinberg::IBStream_::IStreamSeekMode_::{kIBSeekCur, kIBSeekEnd, kIBSeekSet};
        let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        let (bytes, at) = &mut *inner;
        let base = match mode {
            m if m == kIBSeekSet as int32 => 0,
            m if m == kIBSeekCur as int32 => *at as int64,
            m if m == kIBSeekEnd as int32 => bytes.len() as int64,
            _ => return kInvalidArgument,
        };
        let Some(to) = base.checked_add(pos).filter(|to| *to >= 0) else {
            return kInvalidArgument;
        };
        *at = to as usize;
        if !result.is_null() {
            // SAFETY: checked non-null.
            unsafe { result.write(to) };
        }
        kResultOk
    }

    unsafe fn tell(&self, pos: *mut int64) -> tresult {
        if pos.is_null() {
            return kInvalidArgument;
        }
        let at = self.inner.lock().unwrap_or_else(|e| e.into_inner()).1;
        // SAFETY: checked non-null.
        unsafe { pos.write(at as int64) };
        kResultOk
    }
}

impl ISizeableStreamTrait for Stream {
    unsafe fn getStreamSize(&self, size: *mut int64) -> tresult {
        if size.is_null() {
            return kInvalidArgument;
        }
        let len = self.inner.lock().unwrap_or_else(|e| e.into_inner()).0.len();
        // SAFETY: checked non-null.
        unsafe { size.write(len as int64) };
        kResultOk
    }

    unsafe fn setStreamSize(&self, size: int64) -> tresult {
        let Ok(size) = usize::try_from(size) else {
            return kInvalidArgument;
        };
        let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        inner.0.resize(size, 0);
        inner.1 = inner.1.min(size);
        kResultOk
    }
}

/// What a plugin's controller tells the host: edits made in its window,
/// and that it needs restarting. It can outlive the instance, as the
/// plugin holds it.
pub(crate) struct ComponentHandler {
    pub(crate) shared: Weak<Shared>,
}

impl ComponentHandler {
    fn edit(&self, edit: Edit) -> tresult {
        let Some(shared) = self.shared.upgrade() else {
            return kResultFalse;
        };
        shared
            .requests
            .edits
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push(edit);
        shared.wake();
        kResultOk
    }
}

impl Class for ComponentHandler {
    type Interfaces = (IComponentHandler, IComponentHandler2);
}

impl IComponentHandlerTrait for ComponentHandler {
    unsafe fn beginEdit(&self, id: ParamID) -> tresult {
        self.edit(Edit::Begin(id))
    }

    unsafe fn performEdit(&self, id: ParamID, value: ParamValue) -> tresult {
        self.edit(Edit::Value(id, value))
    }

    unsafe fn endEdit(&self, id: ParamID) -> tresult {
        self.edit(Edit::End(id))
    }

    unsafe fn restartComponent(&self, flags: int32) -> tresult {
        let Some(shared) = self.shared.upgrade() else {
            return kResultFalse;
        };
        shared.requests.restart.fetch_or(flags, Ordering::AcqRel);
        shared.wake();
        kResultOk
    }
}

impl IComponentHandler2Trait for ComponentHandler {
    unsafe fn setDirty(&self, state: TBool) -> tresult {
        let Some(shared) = self.shared.upgrade() else {
            return kResultFalse;
        };
        if state != 0 {
            shared.requests.dirty.store(true, Ordering::Release);
            shared.wake();
        }
        kResultOk
    }

    /// The window opens when someone opens it, and only then.
    unsafe fn requestOpenEditor(&self, _: FIDString) -> tresult {
        kResultFalse
    }

    /// Each edit in a group is its own gesture.
    unsafe fn startGroupEdit(&self) -> tresult {
        kResultOk
    }

    unsafe fn finishGroupEdit(&self) -> tresult {
        kResultOk
    }
}

/// What a plugin's window is in, which it asks to resize.
pub(crate) struct PlugFrame {
    pub(crate) shared: Weak<Shared>,
    /// Set while it resizes the view, which a plugin may answer by asking
    /// again.
    resizing: AtomicBool,
}

impl PlugFrame {
    pub(crate) fn new(shared: Weak<Shared>) -> ComWrapper<Self> {
        ComWrapper::new(Self {
            shared,
            resizing: AtomicBool::new(false),
        })
    }
}

impl Class for PlugFrame {
    type Interfaces = (IPlugFrame,);
}

impl IPlugFrameTrait for PlugFrame {
    unsafe fn resizeView(&self, view: *mut IPlugView, size: *mut ViewRect) -> tresult {
        // SAFETY: the plugin passes its view, or null.
        let (Some(view), false) = (unsafe { ComRef::from_raw(view) }, size.is_null()) else {
            return kInvalidArgument;
        };
        let Some(shared) = self.shared.upgrade() else {
            return kResultFalse;
        };
        if self.resizing.swap(true, Ordering::AcqRel) {
            return kResultFalse;
        }
        // SAFETY: checked non-null.
        let wanted = unsafe { *size };
        // As VST3's own host does: the view takes the size at once, if it
        // hasn't already, and the app's window follows it.
        // SAFETY: zeroed is a valid rect.
        let mut now: ViewRect = unsafe { std::mem::zeroed() };
        // SAFETY: on the main thread, with the view's window open.
        let current = unsafe { view.getSize(&mut now) } == kResultOk;
        if !current || !same_rect(&now, &wanted) {
            // SAFETY: as above.
            unsafe { view.onSize(size) };
        }
        self.resizing.store(false, Ordering::Release);
        let (width, height) = rect_size(&wanted);
        shared.request_resize(width, height);
        kResultTrue
    }
}

fn same_rect(a: &ViewRect, b: &ViewRect) -> bool {
    (a.left, a.top, a.right, a.bottom) == (b.left, b.top, b.right, b.bottom)
}

/// A rect's width and height.
pub(crate) fn rect_size(rect: &ViewRect) -> (u32, u32) {
    let side =
        |from: i32, to: i32| (i64::from(to) - i64::from(from)).clamp(0, u32::MAX.into()) as u32;
    (side(rect.left, rect.right), side(rect.top, rect.bottom))
}
